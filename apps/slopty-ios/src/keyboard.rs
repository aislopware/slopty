//! The keyboard's two doors into the shell's key row (`slopty_shell::live::key_row`;
//! `docs/decisions/layout.md`, "iPhone is a list, then the terminal"): whether a hardware
//! keyboard is attached, which takes the touch key row away, and an iPad keyboard's shortcut
//! bar, which holds the row's keys on an iPad laid out as the Mac.
//!
//! UIKit says both on the main thread from callbacks that hold no GPUI context, so what it says
//! goes over a channel to a task on GPUI's foreground executor, which tells the shell. The bar
//! goes the other way: the shell says what it holds, and an observer draws it into the
//! assistant item of `gpui_ios`'s text input view, the first responder the soft keyboard types
//! into.

use std::cell::RefCell;
use std::sync::OnceLock;

use gpui_kit::App;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol};
use objc2::{MainThreadMarker, MainThreadOnly, class, define_class, msg_send, sel};
use objc2_foundation::{NSArray, NSObject, NSString};
use objc2_ui_kit::{UIApplication, UIWindowScene};
use slopty_shell::live::key_row::{self, Armed, Bar, BarKey, BarPress};
use tokio::sync::mpsc;

/// What UIKit said about the keyboard.
#[derive(Clone, Copy, Debug)]
enum Heard {
    /// A hardware keyboard is attached (`true`), or the last one went.
    HardwareKeyboard(bool),
    /// The person tapped a key in the shortcut bar: on its leading edge or its trailing one,
    /// and which.
    Pressed { leading: bool, index: usize },
}

/// Where UIKit's callbacks send what they heard, once the app has started.
static HEARD: OnceLock<mpsc::UnboundedSender<Heard>> = OnceLock::new();

/// A shortcut bar key's tag from this one up names a trailing key, by its index past it.
const TRAILING: isize = 1 << 16;

/// Send what UIKit said to the app. Before the app starts nothing listens, and the start reads
/// the keyboard itself.
fn tell(heard: Heard) {
    if let Some(heard_by) = HEARD.get() {
        // The receiver lives as long as the app.
        let _gone = heard_by.send(heard);
    }
}

/// Start both doors, once, as the app starts: the keyboard as it stands now, every change the
/// system posts, and the bar drawn whenever the shell changes it or the modifiers it shows lit.
pub fn start(cx: &mut App) {
    let (heard_by, mut heard) = mpsc::unbounded_channel();
    if HEARD.set(heard_by).is_err() {
        return;
    }
    key_row::set_hardware_keyboard(slopty_platform::hardware_keyboard_attached(), cx);
    let watch = slopty_platform::keyboard_presence::watch(|attached| {
        tell(Heard::HardwareKeyboard(attached));
    });
    let bar = key_row::observe_bar(cx, |cx| draw_bar(cx));
    let lit = key_row::observe(cx, |cx| draw_bar(cx));
    cx.spawn(async move |cx| {
        let _kept = (watch, bar, lit);
        while let Some(heard) = heard.recv().await {
            cx.update(|cx| match heard {
                Heard::HardwareKeyboard(attached) => key_row::set_hardware_keyboard(attached, cx),
                Heard::Pressed { leading, index } => key_row::press_bar(leading, index, cx),
            });
        }
    })
    .detach();
}

/// The app came back to the front: a keyboard that came or went while it was away may have gone
/// unheard, so it is read again.
pub fn reread() {
    tell(Heard::HardwareKeyboard(slopty_platform::hardware_keyboard_attached()));
}

/// What the bar shows now, and the target its keys call, kept while the app runs: a bar button
/// item holds its target weakly.
struct Drawn {
    bar: Bar,
    armed: Armed,
    target: Retained<Target>,
}

thread_local! {
    static DRAWN: RefCell<Option<Drawn>> = const { RefCell::new(None) };
}

/// Draw the shell's bar into each window's text input view, if it changed: Esc and the modifiers
/// before the system's own keys, those of what runs and the arrows after, an armed modifier lit.
fn draw_bar(cx: &App) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let bar = key_row::bar(cx);
    let armed = key_row::armed(cx);
    let target = DRAWN.with_borrow(|drawn| match drawn {
        // An empty bar lights nothing, so only what it holds can change it.
        Some(drawn) if drawn.bar == bar && (drawn.armed == armed || bar == Bar::default()) => None,
        Some(drawn) => Some(drawn.target.clone()),
        None => Some(Target::new(mtm)),
    });
    let Some(target) = target else {
        return;
    };
    let leading = groups(&bar.leading, 0, armed, &target);
    let trailing = groups(&bar.trailing, TRAILING, armed, &target);
    for view in text_input_views(mtm) {
        // SAFETY: UIKit rule: `-[UIResponder inputAssistantItem]` takes no argument and returns
        // the responder's nonnull `UITextInputAssistantItem`; the view is a `UIView`.
        let item: Retained<AnyObject> = unsafe { msg_send![&*view, inputAssistantItem] };
        // SAFETY: UIKit rule: `-[UITextInputAssistantItem setLeadingBarButtonGroups:]` takes a
        // nonnull `NSArray<UIBarButtonItemGroup *>`, which it copies, and shows the change.
        let () = unsafe { msg_send![&*item, setLeadingBarButtonGroups: &*leading] };
        // SAFETY: as above, for `setTrailingBarButtonGroups:`.
        let () = unsafe { msg_send![&*item, setTrailingBarButtonGroups: &*trailing] };
    }
    DRAWN.set(Some(Drawn { bar, armed, target }));
}

/// One edge's keys as the bar's groups: one group of them, or none for no keys. Each key's tag is
/// `first` plus its index; a modifier's item is lit while it is armed.
fn groups(
    keys: &[BarKey],
    first: isize,
    armed: Armed,
    target: &Target,
) -> Retained<NSArray<AnyObject>> {
    let items: Vec<Retained<AnyObject>> = keys
        .iter()
        .zip(first..)
        .map(|(key, tag)| {
            let title = NSString::from_str(key.face);
            // SAFETY: UIKit rule: `+[UIBarButtonItem alloc]` returns an uninitialised item.
            let item: Allocated<AnyObject> = unsafe { msg_send![class!(UIBarButtonItem), alloc] };
            // SAFETY: UIKit rule: `-[UIBarButtonItem initWithTitle:style:target:action:]` takes a
            // nullable title, a `UIBarButtonItemStyle` (`Plain` is 0), a nullable target it holds
            // weakly and a nullable action, here `press:`, which `Target` answers.
            let item: Retained<AnyObject> = unsafe {
                msg_send![
                    item,
                    initWithTitle: &*title,
                    style: 0_isize,
                    target: target,
                    action: sel!(press:)
                ]
            };
            // SAFETY: UIKit rule: `-[UIBarItem setTag:]` takes an `NSInteger`.
            let () = unsafe { msg_send![&*item, setTag: tag] };
            let name = NSString::from_str(key.name);
            // SAFETY: UIKit rule: `-[NSObject(UIAccessibility) setAccessibilityLabel:]` takes a
            // nullable `NSString`, which it copies.
            let () = unsafe { msg_send![&*item, setAccessibilityLabel: &*name] };
            let lit = matches!(key.press, BarPress::Modifier(modifier) if armed.on(modifier));
            // SAFETY: UIKit rule: `-[UIBarButtonItem setSelected:]` takes a `BOOL`.
            let () = unsafe { msg_send![&*item, setSelected: lit] };
            item
        })
        .collect();
    if items.is_empty() {
        return NSArray::new();
    }
    let items = NSArray::from_retained_slice(&items);
    // SAFETY: UIKit rule: `+[UIBarButtonItemGroup alloc]` returns an uninitialised group.
    let group: Allocated<AnyObject> = unsafe { msg_send![class!(UIBarButtonItemGroup), alloc] };
    // SAFETY: UIKit rule: `-[UIBarButtonItemGroup initWithBarButtonItems:representativeItem:]`
    // takes a nonnull `NSArray<UIBarButtonItem *>` and a nullable representative item.
    let group: Retained<AnyObject> = unsafe {
        msg_send![
            group,
            initWithBarButtonItems: &*items,
            representativeItem: Option::<&AnyObject>::None
        ]
    };
    NSArray::from_retained_slice(&[group])
}

/// Every `gpui_ios` text input view in the app's windows: the hidden view each window makes the
/// first responder while the soft keyboard types into it.
fn text_input_views(mtm: MainThreadMarker) -> Vec<Retained<AnyObject>> {
    let Some(class) = AnyClass::get(c"GPUITextInputView") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for scene in UIApplication::sharedApplication(mtm).connectedScenes() {
        let Some(scene) = scene.downcast_ref::<UIWindowScene>() else {
            continue;
        };
        // SAFETY: UIKit rule: `-[UIWindowScene windows]` takes no argument and returns a
        // nonnull `NSArray<UIWindow *>`.
        let windows: Retained<NSArray<AnyObject>> = unsafe { msg_send![scene, windows] };
        for window in &windows {
            find_views(&window, class, &mut found);
        }
    }
    found
}

/// Collect the views under `view` of `class`, depth first.
fn find_views(view: &AnyObject, class: &AnyClass, found: &mut Vec<Retained<AnyObject>>) {
    // SAFETY: UIKit rule: `-[UIView subviews]` takes no argument and returns a nonnull
    // `NSArray<UIView *>`; every object walked here is a `UIWindow` or a `UIView` under one.
    let subviews: Retained<NSArray<AnyObject>> = unsafe { msg_send![view, subviews] };
    for subview in &subviews {
        if std::ptr::eq(subview.class(), class) {
            found.push(subview);
        } else {
            find_views(&subview, class, found);
        }
    }
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Target` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyKeyBarTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        /// A shortcut bar key's action: its tag says which key, and the app presses it.
        #[unsafe(method(press:))]
        fn press(&self, sender: &AnyObject) {
            // SAFETY: UIKit rule: a bar button item's action is sent with the item, a `UIBarItem`,
            // whose `tag` takes no argument and returns an `NSInteger`.
            let tag: isize = unsafe { msg_send![sender, tag] };
            let (leading, index) = match tag.checked_sub(TRAILING) {
                Some(past) if past >= 0 => (false, past),
                _ => (true, tag),
            };
            if let Ok(index) = usize::try_from(index) {
                tell(Heard::Pressed { leading, index });
            }
        }
    }
);

impl Target {
    /// The one target every bar key calls.
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject rule: `-init` on a freshly allocated instance returns it initialised.
        unsafe { msg_send![super(this), init] }
    }
}
