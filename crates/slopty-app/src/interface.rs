//! The app's window: the shell (`slopty-shell`), over this workspace's workers and threads.
//!
//! The workspace runs as before, with no window of its own: its links, its threads and its
//! terminals are the shell's data, served by Slopty's view model ([`slopty_ui::view_model`]).
//! The shell keeps its own settings, typography, theme and window; the workspace hears the window
//! come forward and go back, as it heard its own, and its views wear the shell's look ([`wear`]).

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AnyWindowHandle, App, Entity, Hsla, SharedString, Subscription};
use slopty_settings::{Loaded, Settings};
use slopty_shell::live::{Chrome, Hosted, TerminalLook};
use slopty_terminal::{TerminalView, TerminalViewEvent};
use slopty_theme::{Rgb, Theme, Tint};
use slopty_ui::view_model::live::{LiveLook, LiveView};
use slopty_ui::workspace::WorkspaceView;

use crate::{Workspace, self_test, settings};

/// The id the shell knows this app by: a client, never one of the devices it lists, so
/// every chat names the worker it runs on.
const CLIENT: &str = "slopty-client";

/// Open the shell's window on `workspace`, its view model served from the workspace, and watch
/// the window for the workspace. `loaded` is the settings read at launch.
pub(crate) fn open(
    workspace: &Entity<Workspace>,
    runtime: tokio::runtime::Handle,
    loaded: Loaded,
    cx: &mut App,
) -> AnyWindowHandle {
    gpui_tokio::init_from_handle(cx, runtime);
    let (view_model, feed, asks) = slopty_ui::view_model::ViewModel::new(CLIENT.to_owned());
    let view = workspace.read(cx).view.clone();
    workspace.update(cx, |ws, cx| {
        ws.interface_feed = Some(std::sync::Arc::clone(&feed));
        ws.tell_server_presence(cx);
        ws.tell_local_device();
    });
    view.update(cx, |v, cx| v.serve_interface(feed, asks, cx));
    host_live_views(&view, cx);
    // The shell's own deep links are not Slopty's: nothing sends on this.
    let (_links, links) = futures::channel::mpsc::unbounded();
    let config = slopty_shell::UiConfig {
        data_dir: slopty_platform::dirs::data_dir().join("interface"),
        default_harness: slopty_view_proto::HarnessId::CLAUDE_CODE,
        initial_url: None,
        service: view_model,
        activate: !self_test(),
    };
    let shell = slopty_shell::start(config, links, cx);
    // The shell's and the kit's bindings are in: Tab and the rest go back to the views that take
    // every key.
    slopty_ui::key_takers::release(cx);
    let watched = workspace.clone();
    let _open = shell.update(cx, move |_shell, window, cx| {
        let observed = watched.downgrade();
        let look = slopty_shell::live::observe_theme(cx, move |cx| {
            let _gone = observed.update(cx, Workspace::rebuild_theme);
        });
        let handle = window.window_handle();
        watched.update(cx, |ws, cx| {
            let activation = cx.observe_window_activation(window, |ws, window, cx| {
                let active = window.is_window_active();
                ws.view.update(cx, |v, cx| v.set_app_active(active, cx));
                ws.set_active(active, cx);
                ws.tell_presence_in(window, cx);
                if active {
                    ws.wake_this_mac_run();
                }
            });
            ws.window_subscriptions = vec![look, activation];
            ws.window = Some(handle);
            // Drags from other apps onto the window: a remote window or a shell takes its own.
            #[cfg(target_os = "macos")]
            ws.view.update(cx, |_, cx| WorkspaceView::accept_dropped_files(window, cx));
            // The clipboard follows the keyboard of this window's live views.
            ws.view.update(cx, |v, cx| v.set_pasteboard(crate::pasteboard(), Some(handle), cx));
            ws.apply_loaded(loaded, cx);
        });
    });
    shell.into()
}

/// Hand the shell the workspace's own views by the ids its view model answers: a shell's
/// terminal and a remote window or desktop (`slopty_shell::live`).
fn host_live_views(view: &Entity<WorkspaceView>, cx: &mut App) {
    let terminals = view.downgrade();
    let screens = view.downgrade();
    let watched = view.downgrade();
    slopty_shell::live::install(
        cx,
        move |id, look, cx| {
            let look = live_look(look);
            let view = terminals.upgrade()?.update(cx, |v, cx| v.hosted_terminal(id, look, cx))?;
            Some(hosted_terminal(view, cx))
        },
        move |id, cx| screens.upgrade()?.update(cx, |v, cx| v.hosted_screen(id, cx)).map(hosted),
        move |id, changed, cx| watch(&watched.upgrade()?, id, changed, cx),
    );
    // A page an agent shows inline is hosted by the workspace, its card drawn by the shell.
    #[cfg(target_vendor = "apple")]
    {
        let (pages, acts, watches) = (view.downgrade(), view.downgrade(), view.downgrade());
        slopty_shell::live::serve_pages(
            cx,
            move |key, window, cx| {
                pages.upgrade()?.update(cx, |v, cx| v.hosted_page(key, window, cx))
            },
            move |key, act, cx| {
                if let Some(view) = acts.upgrade() {
                    view.update(cx, |v, cx| v.page_act(key, act, cx));
                }
            },
            move |key, changed, cx| {
                Some(watches.upgrade()?.update(cx, |v, _| v.page_watch(key, changed)))
            },
        );
    }
    // A terminal opens at the grid its view draws in the size the shell measured for it.
    let fitted = view.downgrade();
    slopty_shell::live::serve_fit(cx, move |size, window, cx| {
        let look = TerminalLook::of(cx);
        let frame =
            slopty_terminal::GridFrame { padding: look.padding, line_height: look.line_height };
        let theme = fitted.upgrade().map(|view| view.read(cx).theme().clone());
        let theme = theme.unwrap_or_default();
        slopty_terminal::fits(&theme, Some(frame), size, window, cx)
    });
    // A split snaps a terminal's pane to the cell its view draws.
    let measured = view.downgrade();
    slopty_shell::live::serve_grid(cx, move |window, cx| {
        let look = TerminalLook::of(cx);
        let frame =
            slopty_terminal::GridFrame { padding: look.padding, line_height: look.line_height };
        let theme = measured.upgrade().map(|view| view.read(cx).theme().clone());
        slopty_terminal::cell(&theme.unwrap_or_default(), Some(frame), window, cx)
    });
    // Whether the person has words in a terminal's line unsent: what an agent driving the
    // window reads as them typing.
    slopty_shell::live::serve_drafting(cx, |view, cx| {
        view.clone().downcast::<TerminalView>().is_ok_and(|terminal| terminal.read(cx).drafting())
    });
    // A split dims its panes without the keyboard, by brightness alone, in their own element.
    slopty_shell::live::serve_dim(cx, |view, dimmed, cx| {
        if let Ok(terminal) = view.clone().downcast::<TerminalView>() {
            terminal.update(cx, |terminal, cx| terminal.set_dimmed(dimmed, cx));
        }
    });
    // A line an orchestrating agent asks to run goes into the shell terminal it opened: typed and
    // run, or left at the prompt for the person to read first.
    let typed = view.downgrade();
    slopty_shell::live::serve_runs(cx, move |id, line, enter, cx| {
        let Some(view) = typed.upgrade() else { return false };
        let look = live_look(&TerminalLook::of(cx));
        let terminal = view
            .update(cx, |v, cx| v.hosted_terminal(id, look, cx))
            .and_then(|live| live.view.downcast::<TerminalView>().ok());
        let Some(terminal) = terminal else { return false };
        let line = line.to_owned();
        terminal.update(cx, |terminal, cx| {
            if enter {
                terminal.run_text(line, cx);
            } else {
                terminal.type_text(line, cx);
            }
        });
        true
    });
}

/// Call `changed` when the view `id` says something new of itself: what the workspace hears (a
/// title, an end), a terminal's progress, which only its tab shows, and its ground, which the
/// titlebar wears.
fn watch(
    view: &Entity<WorkspaceView>,
    id: &str,
    changed: Box<dyn FnMut(&mut App)>,
    cx: &mut App,
) -> Option<Subscription> {
    let changed = Rc::new(RefCell::new(changed));
    let told = Rc::clone(&changed);
    let heard = view.update(cx, |v, cx| {
        v.hosted_watch(id, Box::new(move |cx: &mut App| (told.borrow_mut())(cx)), cx)
    })?;
    let look = live_look(&TerminalLook::of(cx));
    let terminal = view
        .update(cx, |v, cx| v.hosted_terminal(id, look, cx))
        .and_then(|live| live.view.downcast::<TerminalView>().ok());
    let Some(terminal) = terminal else { return Some(heard) };
    let progress = cx.subscribe(&terminal, move |_, event, cx| {
        if matches!(event, TerminalViewEvent::Progress | TerminalViewEvent::Ground(_)) {
            (changed.borrow_mut())(cx);
        }
    });
    Some(Subscription::join(heard, progress))
}

/// The shell's terminal frame, as the workspace takes it.
const fn live_look(look: &TerminalLook) -> LiveLook {
    LiveLook { line_height: look.line_height, padding: look.padding }
}

/// The theme Slopty's own views draw with: the shell's look worn over the file's terminal
/// behaviour, or the light theme before the shell has one.
pub(crate) fn theme(settings: &Settings, cx: &App) -> Theme {
    let Some(chrome) = Chrome::of(cx) else {
        return settings::theme_for(settings, false);
    };
    let mut theme = settings::theme_for(settings, chrome.dark);
    let ui_size = slopty_shell::typography::font_size(cx).pixels() * CHROME_TEXT / DEFAULT_REM;
    wear(&mut theme, &chrome, &TerminalLook::of(cx), ui_size);
    theme
}

/// The shell's chrome text, at its default size: zeron's 13 px rem.
const CHROME_TEXT: f32 = 13.0;

/// The rem the shell's sizes are given at: Settings → Appearance → font size's default.
const DEFAULT_REM: f32 = 16.0;

/// Dress `theme` in the shell's look, so nothing Slopty draws reads as a seam: the chrome's
/// planes, lines, text and states from `chrome`, its type at `ui_size` (Settings → Appearance →
/// font size), the grid's face, size and palette from `look`. A translucent colour of the
/// shell's is laid over its panel where the theme keeps an opaque one, and stays a wash where the
/// theme keeps a [`Tint`]. Slopty's views draw their floats, buttons and icons with the shell's
/// own helpers (`slopty_shell::live`), and their grid's selection, cursor and find hits from
/// `look` itself, translucent; what is worn here is the rest, and the colours a program asks its
/// terminal for.
fn wear(theme: &mut Theme, chrome: &Chrome, look: &TerminalLook, ui_size: f32) {
    let on = |color: Hsla| opaque(color, chrome.panel);
    let s = &mut theme.surfaces;
    s.ground = on(chrome.panel);
    s.sidebar = on(chrome.surface);
    s.tab_bar = s.sidebar;
    s.header = s.sidebar;
    s.elevated = on(chrome.overlay);
    s.well = on(chrome.input);
    s.card_head = on(chrome.card);
    s.element = on(chrome.raised);
    s.hover = tint(chrome.hover);
    s.selected = tint(chrome.active);
    s.keyed = s.selected;
    s.pressed = s.selected;
    s.stroke = tint(chrome.border);
    s.border = tint(chrome.border_strong);
    s.focus = on(chrome.border_strong);
    s.selected_border = s.focus;
    s.drop_target = tint(chrome.accent_wash);
    s.text = on(chrome.text);
    s.text_secondary = on(chrome.text_muted);
    s.text_muted = on(chrome.text_faint);
    s.icon = s.text;
    s.icon_muted = s.text_secondary;
    s.accent = on(chrome.accent);
    s.accent_fill = on(chrome.accent_strong);
    s.accent_ink = opaque(chrome.on_accent, chrome.accent_strong);
    s.info_background = tint(chrome.accent_wash);
    s.working = on(chrome.busy);
    s.success = on(chrome.success);
    s.warn = on(chrome.warning);
    s.error = on(chrome.danger);
    s.error_fill = on(chrome.danger_strong);
    // The states' marks, grounds and edges, as the shell washes its own (no Zed blue left).
    s.success_fill = s.success;
    s.warn_fill = s.warn;
    s.working_fill = s.working;
    s.info_border = on(chrome.accent.opacity(STATE_EDGE));
    s.success_background = tint(chrome.success.opacity(STATE_GROUND));
    s.warning_background = tint(chrome.warning.opacity(STATE_GROUND));
    s.error_background = tint(chrome.danger.opacity(STATE_GROUND));
    s.error_border = on(chrome.danger.opacity(STATE_EDGE));
    s.indent_guide = s.stroke;
    s.line_number = s.text_muted;
    s.line_number_active = s.text_secondary;
    let type_ = &mut theme.typography;
    type_.ui_family = chrome.sans.to_string();
    type_.ui_size = ui_size;
    let family = look.family.to_string();
    type_.mono_families.retain(|f| *f != family);
    type_.mono_families.insert(0, family);
    type_.mono_size = look.font_size;
    // A grid keeps a glyph per cell.
    type_.ligatures = false;
    let bg = look.background.opacity(1.0);
    let palette = &mut theme.terminal;
    palette.bg = opaque(bg, bg);
    palette.fg = opaque(look.foreground, bg);
    // What a program asks its terminal for (OSC 12, 17): the grid paints the translucent
    // cursor and selection from `look` itself.
    palette.cursor = opaque(look.cursor, bg);
    palette.cursor_text = palette.bg;
    palette.selection = opaque(look.selection, bg);
    for (slot, color) in palette.ansi.iter_mut().zip(look.ansi) {
        *slot = opaque(color, bg);
    }
}

/// A state's quiet ground (a failure's, a warning's), as a share of its mark: the shell's error
/// strip's `danger`·0.06 (`settings::widgets::error_strip`).
const STATE_GROUND: f32 = 0.06;

/// A state's edge, as a share of its mark: the error strip's `danger`·0.2 line.
const STATE_EDGE: f32 = 0.2;

/// `color` laid over `under`, opaque.
fn opaque(color: Hsla, under: Hsla) -> Rgb {
    let top = gpui::Rgba::from(color);
    let bottom = gpui::Rgba::from(under.opacity(1.0));
    let mix = |a: f32, b: f32| byte(a.mul_add(top.a, b * (1.0 - top.a)));
    Rgb { r: mix(top.r, bottom.r), g: mix(top.g, bottom.g), b: mix(top.b, bottom.b) }
}

/// `color` as a wash: its ink opaque, its own opacity kept.
fn tint(color: Hsla) -> Tint {
    Tint { ink: opaque(color.opacity(1.0), color), alpha: byte(color.a) }
}

/// A 0–1 channel as a byte.
fn byte(channel: f32) -> u8 {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0..=255")]
    let byte = (channel.clamp(0.0, 1.0) * 255.0).round() as u8;
    byte
}

/// A terminal of the workspace, as the shell places it: also the status its program ended with,
/// how far the program says it has got, what a drop of files on it does, and its ground.
fn hosted_terminal(view: LiveView, cx: &App) -> Hosted {
    let terminal = view.view.clone().downcast::<TerminalView>().ok();
    let mut hosted = hosted(view);
    if let Some(terminal) = terminal {
        let terminal = terminal.read(cx);
        hosted.exit = terminal.exited().flatten();
        hosted.progress = terminal.progress();
        hosted.drop = Some(drop_words(terminal.takes_drops(), terminal.title()));
        hosted.ground = Some(slopty_kit::colors::hsla(terminal.ground()));
    }
    hosted
}

/// What a drop of files on a terminal does: its program takes them when it asks for drops,
/// else their paths are typed.
fn drop_words(takes: bool, title: Option<&str>) -> SharedString {
    match title.map(str::trim).filter(|title| !title.is_empty()) {
        Some(title) if takes => format!("Drop into {title}").into(),
        _ if takes => "Drop into the program".into(),
        _ => "Drop to paste paths".into(),
    }
}

/// A workspace view, as the shell places it.
fn hosted(view: LiveView) -> Hosted {
    Hosted {
        view: view.view,
        focus: view.focus,
        title: view.title,
        ended: view.ended,
        exit: None,
        progress: None,
        drop: None,
        ground: None,
    }
}

#[cfg(test)]
mod tests {
    use gpui::{hsla, rgb};

    use super::*;

    /// A look of the shell's: a dark panel, a translucent hairline and hover, a red danger.
    fn chrome() -> Chrome {
        let grey = |l: f32| hsla(0.0, 0.0, l, 1.0);
        Chrome {
            dark: true,
            sans: "Geist".into(),
            panel: rgb(0x0006_0606).into(),
            surface: grey(0.06),
            raised: grey(0.09),
            card: grey(0.07),
            overlay: grey(0.12),
            input: grey(0.08),
            hover: hsla(0.0, 0.0, 1.0, 0.06),
            active: hsla(0.0, 0.0, 1.0, 0.1),
            border: hsla(0.0, 0.0, 1.0, 0.08),
            border_strong: grey(0.3),
            text: grey(0.95),
            text_muted: grey(0.7),
            text_faint: grey(0.5),
            accent: hsla(0.6, 0.8, 0.6, 1.0),
            accent_strong: hsla(0.6, 0.8, 0.5, 1.0),
            accent_wash: hsla(0.6, 0.8, 0.5, 0.15),
            on_accent: grey(1.0),
            success: hsla(0.4, 0.6, 0.5, 1.0),
            warning: hsla(0.12, 0.8, 0.5, 1.0),
            danger: hsla(0.0, 0.8, 0.6, 1.0),
            danger_strong: hsla(0.0, 0.8, 0.5, 1.0),
            busy: hsla(0.6, 0.8, 0.6, 1.0),
        }
    }

    /// The terminal's look of the same shell: Berkeley Mono at 12 on its own panel.
    fn terminal() -> TerminalLook {
        TerminalLook {
            family: "Berkeley Mono".into(),
            font_size: 12.0,
            line_height: 18.0,
            padding: 8.0,
            background: rgb(0x000a_0a0a).into(),
            foreground: rgb(0x00ee_eeee).into(),
            cursor: rgb(0x00ff_ffff).into(),
            selection: hsla(0.6, 0.8, 0.5, 0.3),
            search_match: hsla(0.6, 0.8, 0.5, 0.15),
            search_current: hsla(0.6, 0.8, 0.5, 0.3),
            faint: hsla(0.0, 0.0, 0.5, 1.0),
            ansi: [rgb(0x00ff_0000).into(); 16],
            dark: true,
        }
    }

    /// What Slopty draws wears the shell's look: its planes and words as opaque colours, its
    /// washes and hairlines at their own opacity, its interface face, and the grid's face,
    /// size and palette, a translucent selection laid over the grid's own panel. The file's
    /// behaviour stays.
    #[test]
    fn slopty_s_views_wear_the_shell_s_look() {
        let mut settings = Settings::default();
        settings.terminal.copy_on_select = true;
        let mut theme = settings::theme_for(&settings, true);
        wear(&mut theme, &chrome(), &terminal(), 13.0);
        let s = &theme.surfaces;
        assert_eq!(s.ground, Rgb::hex(0x0006_0606));
        assert_eq!(theme.variant(), slopty_theme::Variant::Dark);
        assert_eq!((s.stroke.ink, s.stroke.alpha), (Rgb::hex(0x00ff_ffff), 20), "8 % white");
        assert_eq!(s.hover.alpha, 15);
        assert_eq!(s.text, Rgb::hex(0x00f2_f2f2));
        assert_eq!(s.text_muted, Rgb::hex(0x0080_8080), "the faint words");
        assert_eq!(theme.typography.ui_family, "Geist");
        assert_eq!(
            theme.typography.mono_families.first().map(String::as_str),
            Some("Berkeley Mono")
        );
        assert_eq!(theme.typography.mono_size, 12.0);
        assert!(!theme.typography.ligatures, "a glyph per cell");
        let palette = &theme.terminal;
        assert_eq!((palette.bg, palette.fg), (Rgb::hex(0x000a_0a0a), Rgb::hex(0x00ee_eeee)));
        assert_eq!(palette.cursor_text, palette.bg);
        assert_eq!(palette.ansi[3], Rgb::hex(0x00ff_0000));
        let wash = opaque(hsla(0.6, 0.8, 0.5, 0.3), rgb(0x000a_0a0a).into());
        assert_eq!(palette.selection, wash, "over the grid's own panel");
        assert_ne!(palette.selection, opaque(hsla(0.6, 0.8, 0.5, 1.0), rgb(0).into()));
        assert!(theme.behaviour.copy_on_select, "the file's behaviour stays");
        assert!((theme.typography.ui_size - 13.0).abs() < f32::EPSILON, "the shell's type size");
        // No Zed blue left: a state's ground, edge and fill are the shell's own marks.
        assert_eq!(s.error_border, opaque(hsla(0.0, 0.8, 0.6, 0.2), rgb(0x0006_0606).into()));
        assert_eq!(s.error_background.alpha, byte(0.06));
        assert_eq!(s.success_fill, s.success);
        assert_eq!(s.info_border, opaque(hsla(0.6, 0.8, 0.6, 0.2), rgb(0x0006_0606).into()));
    }

    /// What a drop on a terminal does reads as its view says: into the program asking for drops,
    /// by its title when it has one, else the paths typed.
    #[test]
    fn a_terminal_names_what_a_drop_does() {
        assert_eq!(drop_words(true, Some("yazi")).as_ref(), "Drop into yazi");
        assert_eq!(drop_words(true, Some("  ")).as_ref(), "Drop into the program");
        assert_eq!(drop_words(false, Some("zsh")).as_ref(), "Drop to paste paths");
    }
}
