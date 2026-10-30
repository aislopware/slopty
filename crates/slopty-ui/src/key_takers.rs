//! The views that take every key: a terminal's program and a remote window's worker.
//!
//! GPUI runs a key's binding before any view hears the key, and gpui-kit binds Tab and ⇧Tab
//! around every view (its window root, `Root`) to walk the focus, and ⌃C to copy off macOS. So
//! Tab would walk the focus out of a shell instead of completing a word, and ⌃C would copy
//! instead of interrupting the program. [`release`] unbinds each chord without ⌘ bound around
//! every view, by its action's name, inside those views alone, so the kit's fields and menus,
//! and every chord with ⌘, keep theirs.

use std::rc::Rc;

use gpui::{App, KeyBinding, KeyBindingContextPredicate};

/// gpui-kit's window root (`gpui_base::Root`), the context around every view.
const KIT_ROOT: &str = "Root";

/// The contexts of the views that take every key.
const KEY_TAKERS: [&str; 2] = [slopty_terminal::CTX, crate::screen::CTX];

/// Give the chords without ⌘ that are bound around every view now back to the views that take
/// every key. Run once the app's bindings are in (the shell's and the kit's).
pub fn release(cx: &mut App) {
    let bound = cx.key_bindings().borrow().bindings().cloned().collect::<Vec<_>>();
    let released = released(&bound, cx);
    cx.bind_keys(released);
}

/// The unbindings that give `bound`'s chords around every view back to [`KEY_TAKERS`].
fn released(bound: &[KeyBinding], cx: &App) -> Vec<KeyBinding> {
    let Ok(root) = gpui::KeyContext::parse(KIT_ROOT) else { return Vec::new() };
    let around = [root];
    let mut out = Vec::new();
    for other in bound {
        let [stroke] = other.keystrokes() else { continue };
        let stroke = stroke.inner();
        let action = other.action();
        if stroke.modifiers.platform
            || gpui::is_no_action(action)
            || gpui::is_unbind(action)
            || !other.predicate().is_none_or(|p| p.eval(&around))
        {
            continue;
        }
        let chord = stroke.unparse(cx.platform_info().os);
        let unbind = gpui::Unbind(action.name().into());
        for context in KEY_TAKERS {
            let predicate = match KeyBindingContextPredicate::parse(context) {
                Ok(predicate) => Rc::new(predicate),
                Err(e) => {
                    tracing::error!(context, error = %e, "release");
                    continue;
                }
            };
            match KeyBinding::load(
                &chord,
                Box::new(unbind.clone()),
                Some(predicate),
                false,
                None,
                cx,
            ) {
                Ok(binding) => out.push(binding),
                Err(e) => tracing::error!(chord, context, error = %e, "release"),
            }
        }
    }
    out
}
