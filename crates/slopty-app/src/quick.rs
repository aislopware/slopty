//! The quick terminal on the app's side: the settings' `[quick_terminal]` handed to the
//! workspace, and its chord registered with macOS so it works from any app.
//!
//! A press arrives on the main run loop and is sent, with the moment it arrived, to a task that
//! toggles the panel; the workspace times the show from that moment. Under the self-test no
//! chord is registered: the machine's keyboard belongs to whoever is using it, and the palette's
//! command reaches the same toggle.

use std::time::Instant;

use gpui::{App, Entity};
use slopty_platform::hotkey::Chord;
use slopty_settings::{QuickTerminalSettings, bounds};
use slopty_ui::palette::PaletteItem;
use slopty_ui::workspace::{
    QuickConfig, QuickToggle, TOGGLE_QUICK_TERMINAL, ToggleQuickTerminal, WorkspaceView,
};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

/// Where the panel sits and whether it hides: a height out of bounds is a typo, and the
/// default applies.
pub(crate) fn config(settings: &QuickTerminalSettings) -> QuickConfig {
    let percent = if bounds::QUICK_HEIGHT.contains(&settings.height) {
        settings.height
    } else {
        QuickTerminalSettings::default().height
    };
    QuickConfig {
        height: f32::from(percent) / 100.0,
        autohide: settings.autohide,
        keyboard: !crate::self_test(),
    }
}

/// The chord the settings name, if they name one that can be registered.
pub(crate) fn chord(settings: &QuickTerminalSettings) -> Result<Option<Chord>, String> {
    let text = settings.hotkey.trim();
    if text.is_empty() {
        return Ok(None);
    }
    Chord::parse(text).map(Some).map_err(|e| format!("Quick terminal chord \"{text}\": {e}"))
}

/// The palette's line for the quick terminal, showing the chord that runs it from any app.
pub(crate) fn palette_line(chord: Option<Chord>) -> Option<PaletteItem> {
    if !WorkspaceView::quick_terminal_offered() {
        return None;
    }
    let bindings: Vec<gpui::KeyBinding> = chord
        .map(|chord| gpui::KeyBinding::new(&chord.keys(), ToggleQuickTerminal, None))
        .into_iter()
        .collect();
    Some(PaletteItem::new(
        TOGGLE_QUICK_TERMINAL,
        slopty_ui::icons::IconName::SquareTerminal,
        Box::new(ToggleQuickTerminal),
        &bindings,
    ))
}

/// Toggle the quick terminal in `view` at every press sent to the channel this returns.
pub(crate) fn listen(view: Entity<WorkspaceView>, cx: &App) -> UnboundedSender<Instant> {
    let (presses, mut pressed) = unbounded_channel::<Instant>();
    cx.spawn(async move |cx| {
        while let Some(at) = pressed.recv().await {
            view.update(cx, |v, cx| v.toggle_quick_terminal(at, QuickToggle::Chord, cx));
        }
    })
    .detach();
    presses
}

/// The chord registered now, and where its presses go.
pub(crate) struct Hotkey {
    presses: UnboundedSender<Instant>,
    /// The chord asked for last, registered or not, so the same settings do not register
    /// again.
    asked: Option<Chord>,
    /// The settings' chord as last written, so what is wrong with it is said once.
    written: Option<String>,
    #[cfg(target_os = "macos")]
    registered: Option<slopty_platform::hotkey::Hotkey>,
}

impl std::fmt::Debug for Hotkey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hotkey").field("asked", &self.asked).finish_non_exhaustive()
    }
}

impl Hotkey {
    /// Nothing registered yet; presses go to `presses`.
    pub(crate) const fn new(presses: UnboundedSender<Instant>) -> Self {
        Self {
            presses,
            asked: None,
            written: None,
            #[cfg(target_os = "macos")]
            registered: None,
        }
    }

    /// Take the settings' chord: register it in place of the one before, one that does not read
    /// letting it go. Returns the chord, and what to tell the human when it does not read or
    /// macOS would not take it, only when the settings wrote it differently than last time.
    pub(crate) fn apply(
        &mut self,
        settings: &QuickTerminalSettings,
    ) -> (Option<Chord>, Option<String>) {
        let fresh = self.written.as_deref() != Some(settings.hotkey.as_str());
        self.written = Some(settings.hotkey.clone());
        let (chord, unread) = match chord(settings) {
            Ok(chord) => (chord, None),
            Err(why) => (None, Some(why)),
        };
        let refused = self.set(chord);
        (chord, unread.or(refused).filter(|_| fresh))
    }

    /// The chord asked for last.
    #[cfg(test)]
    pub(crate) const fn asked(&self) -> Option<Chord> {
        self.asked
    }

    /// Register `chord` in place of the one before (none lets it go). Returns what to tell the
    /// human when macOS would not take it.
    fn set(&mut self, chord: Option<Chord>) -> Option<String> {
        if self.asked == chord {
            return None;
        }
        self.asked = chord;
        #[cfg(target_os = "macos")]
        {
            self.registered = None;
            let chord = chord.filter(|_| !crate::self_test())?;
            match slopty_platform::hotkey::Hotkey::register(chord, self.presses.clone()) {
                Ok(hotkey) => {
                    tracing::info!(keys = chord.keys(), "quick terminal chord registered");
                    self.registered = Some(hotkey);
                    None
                }
                Err(e) => {
                    tracing::warn!(keys = chord.keys(), error = %e, "quick terminal chord");
                    Some(format!("Quick terminal chord {}: {e}", chord.keys()))
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _: &UnboundedSender<Instant> = &self.presses;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings reach the workspace as a share of the screen, a height out of bounds
    /// reading as the default; an empty chord is none, and a wrong one says why.
    #[test]
    fn the_quick_terminal_follows_its_settings() {
        let mut s = QuickTerminalSettings::default();
        assert_eq!(config(&s), QuickConfig::default());
        s.height = 75;
        s.autohide = false;
        assert_eq!((config(&s).height, config(&s).autohide), (0.75, false));
        s.height = 5;
        assert_eq!(config(&s), QuickConfig { autohide: false, ..QuickConfig::default() }, "a typo");
        assert_eq!(chord(&s).map(|c| c.map(Chord::keys)), Ok(Some("ctrl-`".to_owned())));
        s.hotkey = "  ".into();
        assert_eq!(chord(&s), Ok(None), "no chord, only the palette's command");
        s.hotkey = "alt-q".into();
        assert!(chord(&s).is_err_and(|e| e.contains("cmd or ctrl")));
    }
}
