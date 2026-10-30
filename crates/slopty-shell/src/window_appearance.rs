//! The window wears its ground's appearance (`docs/decisions/layout.md`, "The window wears its
//! ground's appearance").
//!
//! Ghostty gives a terminal window the appearance of the background its terminal paints: aqua
//! when the background's luma is past half, dark aqua below it (`TerminalWindow.syncAppearance`,
//! `NSColor.isLightColor`). The traffic lights, menus, popovers and scrollers AppKit draws then
//! read on the ground the program paints, a dark TUI in a light app included; on iOS the status
//! bar and the keyboard do. Slopty does the same for each window: the ground of the session it
//! shows, its program's (OSC 11) else the theme's, sets the window's appearance, and the window
//! carries it as its own only while it is not already the application's. The theme follows the
//! application, never a window ([`crate::appearance`]), so a window's own appearance never turns
//! the theme, and over no session the window wears the theme's appearance.

use gpui::{App, Hsla, Window, WindowAppearance};

use crate::theme::{Appearance, Theme};

/// The appearance a ground reads as, as Ghostty picks a window's from its terminal's background:
/// light when its luma (Rec. 601 weights on its sRGB components) is past half.
pub fn of_ground(ground: Hsla) -> Appearance {
    let rgb = ground.to_rgb();
    if 0.299 * rgb.r + 0.587 * rgb.g + 0.114 * rgb.b > 0.5 {
        Appearance::Light
    } else {
        Appearance::Dark
    }
}

/// The window's own appearance over `ground` (none: over no session), or `None` where the
/// application's already is the one it wants. Over no session the window wants the theme's,
/// which is the application's wherever the platform can set the application's (macOS); on iOS,
/// where it cannot, a pinned theme against the system's style is the window's own.
pub fn own(ground: Option<Hsla>, theme: Appearance, app: Appearance) -> Option<WindowAppearance> {
    let wanted = ground.map_or(theme, of_ground);
    (wanted != app).then_some(match wanted {
        Appearance::Light => WindowAppearance::Light,
        Appearance::Dark => WindowAppearance::Dark,
    })
}

/// What a window was last given, so a frame over the same ground asks the platform nothing.
#[derive(Debug, Default)]
pub struct WindowGround {
    worn: Option<Option<WindowAppearance>>,
}

impl WindowGround {
    /// Gives `window` the appearance of `ground`, the ground of the session it shows, or with
    /// `None`, over no session, the theme's. Called as the window is drawn: a program setting
    /// its background, another session shown, the theme or the application's appearance turning
    /// each draw the window again.
    pub fn follow(&mut self, ground: Option<Hsla>, window: &Window, cx: &App) {
        let own = own(ground, Theme::of(cx).appearance, crate::appearance::app(cx));
        if self.worn != Some(own) {
            self.worn = Some(own);
            window.set_appearance(own);
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Entity, IntoElement, Render, TestAppContext, VisualTestContext};
    use tempfile::TempDir;
    use slopty_shell_theme::{AccentSelection, SurfacePreference, ThemeSelection};

    use super::*;
    use crate::appearance::{self, AppearanceMode};

    /// Solarized's two backgrounds: base03 #002b36 and base3 #fdf6e3.
    fn base03() -> Hsla {
        gpui::rgb(0x002b36).into()
    }

    fn base3() -> Hsla {
        gpui::rgb(0xfdf6e3).into()
    }

    /// A window that wears the ground it is handed, as the shell's does.
    struct Grounded {
        ground: Option<Hsla>,
        worn: WindowGround,
    }

    impl Render for Grounded {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.worn.follow(self.ground, window, cx);
            gpui::div()
        }
    }

    /// The app in `mode`, and a window over no session; the settings the mode is written to
    /// live in the directory, kept for as long as the test runs.
    fn start(mode: AppearanceMode, cx: &mut TestAppContext) -> (TempDir, Entity<Grounded>, &mut VisualTestContext) {
        let dir = tempfile::tempdir().expect("a settings directory");
        cx.update(|cx| {
            gpui_base::init(cx);
            crate::settings::init(Default::default(), dir.path(), cx);
            appearance::init(mode, ThemeSelection::default(), AccentSelection::default(), SurfacePreference::Opaque, cx);
        });
        let (view, cx) = cx.add_window_view(|_, _| Grounded { ground: None, worn: WindowGround::default() });
        (dir, view, cx)
    }

    fn show(view: &Entity<Grounded>, ground: Option<Hsla>, cx: &mut VisualTestContext) {
        view.update(cx, |view, cx| {
            view.ground = ground;
            cx.notify();
        });
        cx.run_until_parked();
    }

    fn window_appearance(cx: &mut VisualTestContext) -> Appearance {
        cx.update(|window, _| Appearance::from_window(window.appearance()))
    }

    fn theme(cx: &mut VisualTestContext) -> Appearance {
        cx.update(|_, cx| Theme::of(cx).appearance)
    }

    /// A dark program in a light app darkens its window and leaves the theme light; the theme's
    /// own ground gives the window back to the app.
    #[gpui::test]
    fn a_dark_ground_darkens_the_window_and_not_the_theme(cx: &mut TestAppContext) {
        let (_dir, view, cx) = start(AppearanceMode::Light, cx);
        show(&view, Some(base03()), cx);
        assert_eq!(window_appearance(cx), Appearance::Dark, "the window wears the program's dark");
        assert_eq!(theme(cx), Appearance::Light, "the theme stays the app's");
        assert_eq!(cx.update(|_, cx| crate::appearance::app(cx)), Appearance::Light, "so does the app");

        let own_ground = cx.update(|_, cx| Theme::of(cx).terminal.background);
        show(&view, Some(own_ground), cx);
        assert_eq!(window_appearance(cx), Appearance::Light, "the theme's own ground clears it");
        assert_eq!(view.read_with(cx, |view, _| view.worn.worn), Some(None), "nothing of its own");
    }

    /// A light program in a dark app lightens its window and leaves the theme dark; over no
    /// session the window is the app's again.
    #[gpui::test]
    fn a_light_ground_lightens_the_window_and_not_the_theme(cx: &mut TestAppContext) {
        let (_dir, view, cx) = start(AppearanceMode::Dark, cx);
        show(&view, Some(base3()), cx);
        assert_eq!(window_appearance(cx), Appearance::Light);
        assert_eq!(theme(cx), Appearance::Dark);

        show(&view, None, cx);
        assert_eq!(window_appearance(cx), Appearance::Dark, "over no session, the app's");
        assert_eq!(view.read_with(cx, |view, _| view.worn.worn), Some(None));
    }

    /// The person's setting and the system's light and dark turn the theme, whatever the window
    /// wears of its own; and the window's own follows the theme it is measured against.
    #[gpui::test]
    fn the_theme_follows_the_app_whatever_the_window_wears(cx: &mut TestAppContext) {
        let (_dir, view, cx) = start(AppearanceMode::System, cx);
        show(&view, Some(base03()), cx);
        assert_eq!((theme(cx), window_appearance(cx)), (Appearance::Light, Appearance::Dark));

        // The system turning dark turns the theme though the window was dark already; the
        // window's dark is the app's now, not its own.
        cx.simulate_system_appearance(WindowAppearance::Dark);
        cx.run_until_parked();
        assert_eq!((theme(cx), window_appearance(cx)), (Appearance::Dark, Appearance::Dark));
        assert_eq!(view.read_with(cx, |view, _| view.worn.worn), Some(None));

        // The person pinning light turns the theme back, against the dark system, and the dark
        // program's window is dark of its own again.
        cx.update(|_, cx| appearance::set_mode(AppearanceMode::Light, cx));
        cx.run_until_parked();
        assert_eq!((theme(cx), window_appearance(cx)), (Appearance::Light, Appearance::Dark));
        assert_eq!(view.read_with(cx, |view, _| view.worn.worn), Some(Some(WindowAppearance::Dark)));

        // Following the system again is the system's dark.
        cx.update(|_, cx| appearance::set_mode(AppearanceMode::System, cx));
        cx.run_until_parked();
        assert_eq!(theme(cx), Appearance::Dark);
    }

    /// A ground reads as Ghostty reads it, by luma, not by lightness; and the window carries
    /// its own appearance only where the app's is not the one it wants.
    #[test]
    fn the_ground_is_read_by_its_luma() {
        assert_eq!(of_ground(base03()), Appearance::Dark);
        assert_eq!(of_ground(base3()), Appearance::Light);
        // Pure blue is light by HSL's lightness (0.5) but dark by luma (0.114).
        assert_eq!(of_ground(gpui::rgb(0x0000ff).into()), Appearance::Dark);
        // Pure yellow's luma is 0.886.
        assert_eq!(of_ground(gpui::rgb(0xffff00).into()), Appearance::Light);
        assert_eq!(own(Some(base03()), Appearance::Light, Appearance::Light), Some(WindowAppearance::Dark));
        assert_eq!(own(Some(base03()), Appearance::Dark, Appearance::Dark), None);
        assert_eq!(own(Some(base3()), Appearance::Dark, Appearance::Dark), Some(WindowAppearance::Light));
        assert_eq!(own(None, Appearance::Light, Appearance::Light), None);
        assert_eq!(
            own(None, Appearance::Light, Appearance::Dark),
            Some(WindowAppearance::Light),
            "a pinned theme against a system the platform cannot pin (iOS)"
        );
    }
}
