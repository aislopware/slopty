// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The chrome's icons and the asset source that serves them.
//!
//! Most glyphs are Solar Icons, Linear weight, by 480 Design, under CC BY 4.0
//! (<https://creativecommons.org/licenses/by/4.0/>; "Solar Icons by 480 Design", attributed in
//! `THIRD_PARTY_NOTICES.md`); zeron drew the rest (terminal, stop, return, the expand arrows and
//! their kin) in the same weight. File icons are Symbols ([`crate::file_icons`]). An agent's mark
//! is Slopty's own ([`crate::icons::AgentMark`]).
//!
//! An icon draws with [`icon`]: `icon(solar::PAPERCLIP).size(px(16.0)).text_color(…)`. GPUI's SVG
//! painter tints by the icon's own text colour, so always set one.

use std::borrow::Cow;

use gpui::{
    AssetSource, Div, Hsla, ParentElement as _, Result, SharedString, Styled as _, Svg, div, px,
    svg,
};

macro_rules! icon_assets {
    ($(($const_name:ident, $path:literal)),+ $(,)?) => {
        $(
            #[doc = concat!("`", $path, "`")]
            pub const $const_name: &str = concat!("solar/", $path, ".svg");
        )+

        /// Every icon path, for the asset source's listing and its test.
        pub const ALL: &[&str] = &[$(concat!("solar/", $path, ".svg")),+];

        fn embedded(path: &str) -> Option<&'static [u8]> {
            match path {
                $(concat!("solar/", $path, ".svg") => Some(
                    include_bytes!(concat!("../assets/solar/", $path, ".svg")).as_slice(),
                ),)+
                _ => None,
            }
        }
    };
}

icon_assets![
    (ACTION_BUILD, "action-build"),
    (ACTION_CONFIGURE, "action-configure"),
    (ACTION_DEBUG, "action-debug"),
    (ACTION_LINT, "action-lint"),
    (ACTION_PLAY, "action-play"),
    (ACTION_TEST, "action-test"),
    (ADD_CIRCLE, "add-circle"),
    (ALT_ARROW_DOWN, "alt-arrow-down"),
    (ALT_ARROW_LEFT, "alt-arrow-left"),
    (ALT_ARROW_RIGHT, "alt-arrow-right"),
    (ALT_ARROW_UP, "alt-arrow-up"),
    (ARCHIVE_MINIMALISTIC, "archive-minimalistic"),
    (ARCHIVE_UP_MINIMALISTIC, "archive-up-minimalistic"),
    (ARROW_DOWN, "arrow-down"),
    (ARROW_LEFT, "arrow-left"),
    (ARROW_RIGHT, "arrow-right"),
    (ARROW_UP, "arrow-up"),
    (ARROW_UP_RIGHT, "arrow-up-right"),
    (BELL, "bell"),
    (BOT, "bot"),
    (CALENDAR, "calendar"),
    (CHAT_ROUND_LINE, "chat-round-line"),
    (CHECK, "check"),
    (CHECKLIST, "checklist"),
    (CLOCK_CIRCLE, "clock-circle"),
    (CLOSE, "close"),
    (CLOSE_CIRCLE, "close-circle"),
    (CLOUD, "cloud"),
    (COLLAPSE_ARROWS, "collapse-arrows"),
    (COMMAND, "command"),
    (COPY, "copy"),
    (DANGER_TRIANGLE, "danger-triangle"),
    (DOCUMENT, "document"),
    (DOCUMENT_ADD, "document-add"),
    (DRAG_HANDLE, "drag-handle"),
    (EXPAND_ARROWS, "expand-arrows"),
    (EYE, "eye"),
    (EYE_CLOSED, "eye-closed"),
    (FAST_TIER, "fast-tier"),
    (FAST_TIER_BOLD, "fast-tier-bold"),
    (FILE_CODE, "file-code"),
    (FILE_DATA, "file-data"),
    (FILE_IMAGE, "file-image"),
    (FILE_MARKDOWN, "file-markdown"),
    (FILE_STYLE, "file-style"),
    (FILE_TREE, "file-tree"),
    (FLOPPY_DISK, "floppy-disk"),
    (FOLD_VERTICAL, "fold-vertical"),
    (FOLDER, "folder"),
    (FOLDER_TREE, "folder-tree"),
    (FOLDER_WITH_FILES, "folder-with-files"),
    (FORK, "fork"),
    (GALLERY, "gallery"),
    (GIT_BRANCH, "git-branch"),
    (GLOBAL, "global"),
    (GLOBE, "globe"),
    (HARD_DRIVE, "hard-drive"),
    (HOME, "home"),
    (INFO_CIRCLE, "info-circle"),
    (KEY_MINIMALISTIC, "key-minimalistic"),
    (KEYBOARD, "keyboard"),
    (LAPTOP, "laptop"),
    (LIST, "list"),
    (LOGOUT_2, "logout-2"),
    (MAGIC_STICK_3, "magic-stick-3"),
    (MAGNIFER, "magnifer"),
    (MONITOR, "monitor"),
    (MOON, "moon"),
    (MORE_HORIZONTAL, "more-horizontal"),
    (PALETTE_SEARCH, "palette-search"),
    (PAPERCLIP, "paperclip"),
    (PEN, "pen"),
    (PEN_NEW_SQUARE, "pen-new-square"),
    (PIN, "pin"),
    (PLUS, "plus"),
    (PROJECT_DEFAULT, "project-default"),
    (PULL_REQUEST, "pull-request"),
    (PULL_REQUEST_DRAFT, "pull-request-draft"),
    (QUEUE_CHECK, "queue-check"),
    (QUEUE_CLOSE, "queue-close"),
    (QUEUE_DRAG_HANDLE, "queue-drag-handle"),
    (QUEUE_PAPERCLIP, "queue-paperclip"),
    (QUEUE_SEND, "queue-send"),
    (REFRESH, "refresh"),
    (REMOTE_SERVER, "remote-server"),
    (RESTART, "restart"),
    (RETURN, "return"),
    (SETTINGS, "settings"),
    (SETTINGS_MINIMALISTIC, "settings-minimalistic"),
    (SMARTPHONE, "smartphone"),
    (SORT, "sort"),
    (SORT_VERTICAL, "sort-vertical"),
    (SPLIT_COLUMNS, "split-columns"),
    (STAR, "star"),
    (STAR_BOLD, "star-bold"),
    (STOP, "stop"),
    (SUN, "sun"),
    (TAG, "tag"),
    (TERMINAL, "terminal"),
    (TRASH_BIN_MINIMALISTIC, "trash-bin-minimalistic"),
    (TUNING, "tuning"),
    (VOLUME_LOUD, "volume-loud"),
    (WIDGET, "widget"),
    (WIFI_OFF, "wifi-off"),
    (WINDOW_MAXIMIZE, "window-maximize"),
    (WINDOW_MINIMIZE, "window-minimize"),
    (WINDOW_RESTORE, "window-restore"),
    (WORKTREE, "worktree"),
    (WRAP_TEXT, "wrap-text"),
];

/// Serves the icons, the file icons, and gpui-kit's own assets after them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = embedded(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(asset) = crate::file_icons::Assets.load(path)? {
            return Ok(Some(asset));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut assets: Vec<SharedString> =
            ALL.iter().filter(|p| p.starts_with(path)).map(|p| SharedString::from(*p)).collect();
        assets.extend(crate::file_icons::Assets.list(path)?);
        assets.extend(gpui_kit::assets::Assets.list(path)?);
        Ok(assets)
    }
}

/// An icon element for an embedded path; the caller sets its size and text colour.
#[must_use]
pub fn icon(path: &'static str) -> Svg {
    svg().path(path).flex_none()
}

/// The sidebar toggle.
///
/// A rounded frame holds a panel whose width morphs from 5.5 to 1.75 as the sidebar closes, drawn
/// from quads in the glyph's 24-unit space and scaled to `size`. `open` is the morph's progress
/// ([`crate::motion::state_t`]); `right` mirrors it.
#[must_use]
pub fn sidebar_glyph(open: f32, right: bool, size: f32, color: Hsla) -> Div {
    let s = size / 24.0;
    let inset = px(6.5 * s);
    let panel = div()
        .absolute()
        .top(px(7.5 * s))
        .w(px(crate::motion::lerp(1.75, 5.5, open.clamp(0.0, 1.0)) * s))
        .h(px(9.0 * s))
        .rounded(px(0.875 * s))
        .bg(color);
    let panel = if right { panel.right(inset) } else { panel.left(inset) };
    div().relative().flex_none().size(px(size)).child(panel_frame(s, color)).child(panel)
}

/// The frame every panel toggle shares, from quads so its strokes stay crisp at 16 px.
fn panel_frame(s: f32, color: Hsla) -> Div {
    let stroke = 1.75_f32;
    div()
        .absolute()
        .left(px((stroke / 2.0).mul_add(-s, 3.0 * s)))
        .top(px((stroke / 2.0).mul_add(-s, 4.0 * s)))
        .w(px((18.0 + stroke) * s))
        .h(px((16.0 + stroke) * s))
        .rounded(px((4.0 + stroke / 2.0) * s))
        .border(px(stroke * s))
        .border_color(color)
}

/// The session card toggle: the frame with a panel in its top-right corner.
#[must_use]
pub fn corner_card_glyph(size: f32, color: Hsla) -> Div {
    let s = size / 24.0;
    let panel = div()
        .absolute()
        .top(px(7.5 * s))
        .right(px(6.5 * s))
        .w(px(5.5 * s))
        .h(px(4.5 * s))
        .rounded(px(0.875 * s))
        .bg(color);
    div().relative().flex_none().size(px(size)).child(panel_frame(s, color)).child(panel)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every icon is served, and parses as an SVG.
    #[test]
    fn every_icon_is_served() {
        for path in ALL {
            let bytes = Assets.load(path).expect("loads").expect("present");
            let text = std::str::from_utf8(&bytes).expect("utf-8");
            assert!(text.contains("<svg"), "{path}");
        }
        assert_eq!(Assets.list("solar/").expect("lists").len(), ALL.len(), "listed");
    }
}
