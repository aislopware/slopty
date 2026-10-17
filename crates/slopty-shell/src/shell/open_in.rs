//! slopty: a chat's folder in the person's own editor (T3 Code's "Open in"). A folder on this Mac
//! opens with `open -a <app>`; a folder on another machine opens through the editor's SSH remote,
//! at the address this Mac reaches it by (`Device::ssh_host`): VS Code and Cursor's
//! `vscode-remote/ssh-remote+<host>` link, Zed's `zed://ssh/<host>`. Only the editors installed
//! here are offered, Finder for a folder on this Mac. Every open goes through [`launch`], which an
//! end-to-end run points at a file (`SLOPTY_E2E_OPEN_LOG`) so nothing is launched.

use std::cell::Cell;

use gpui::{
    AnyElement, App, Bounds, Context, IntoElement, ParentElement, Pixels, Point, SharedString,
    Styled, div, prelude::*, px,
};

use super::Shell;
use crate::icons::{self, icon};
use crate::popover;
use crate::settings::widgets::{ActionHint as _, Keys};
use crate::theme::Theme;

thread_local! {
    /// Where the titlebar's Open in button was last drawn: the menu hangs under it, whether a
    /// click or ⌥⌘O asked for it.
    static BUTTON: Cell<Option<Bounds<Pixels>>> = const { Cell::new(None) };
}

/// Under the button, its menu's top-left: below it by the menus' gap, its left edge aligned.
fn under_button() -> Option<Point<Pixels>> {
    BUTTON.with(Cell::get).map(|bounds| gpui::point(bounds.left(), bounds.bottom() + px(4.0)))
}

/// Whether `host` names this machine: a worker reached over loopback runs here, where Finder and
/// the editors open its folders without SSH.
fn on_this_machine(host: &str) -> bool {
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// `path` with a leading `~` spelled out as `home`, as `open -a` takes it (no shell expands it).
fn expand_home(path: &str, home: Option<&str>) -> String {
    match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", home.trim_end_matches('/'))
        }
        _ => path.to_owned(),
    }
}

/// Where the person's own editors and Finder are: what [`launch`] opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Editor {
    VsCode,
    Cursor,
    Zed,
    Finder,
}

impl Editor {
    const ALL: [Editor; 4] = [Editor::VsCode, Editor::Cursor, Editor::Zed, Editor::Finder];

    /// Its name, as macOS knows the app.
    fn app(self) -> &'static str {
        match self {
            Editor::VsCode => "Visual Studio Code",
            Editor::Cursor => "Cursor",
            Editor::Zed => "Zed",
            Editor::Finder => "Finder",
        }
    }

    /// Whether this Mac has it.
    fn installed(self) -> bool {
        if self == Editor::Finder {
            return true;
        }
        let bundle = format!("{}.app", self.app());
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        std::iter::once(std::path::PathBuf::from("/Applications"))
            .chain(home.map(|home| home.join("Applications")))
            .any(|dir| dir.join(&bundle).exists())
    }
}

/// What opening a folder in an editor runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Open {
    /// A link the editor answers.
    Url(String),
    /// `open -a <app> <path>`.
    App { app: &'static str, path: String },
}

impl Open {
    /// The line an end-to-end run reads back.
    fn said(&self) -> String {
        match self {
            Open::Url(url) => url.clone(),
            Open::App { app, path } => format!("open -a {app} {path}"),
        }
    }
}

/// How `editor` opens `path`: on this Mac when `ssh_host` is `None`, else on the machine this Mac
/// reaches at `ssh_host`. `None` where the editor cannot (Finder on another machine).
pub(crate) fn open(editor: Editor, path: &str, ssh_host: Option<&str>) -> Option<Open> {
    let Some(host) = ssh_host.filter(|host| !host.is_empty()) else {
        return Some(Open::App { app: editor.app(), path: path.to_owned() });
    };
    let host = encode(host, b"@:.-_~");
    let path = encode(if path.starts_with('/') { path } else { return None }, b"/.-_~");
    Some(Open::Url(match editor {
        Editor::VsCode => format!("vscode://vscode-remote/ssh-remote+{host}{path}"),
        Editor::Cursor => format!("cursor://vscode-remote/ssh-remote+{host}{path}"),
        Editor::Zed => format!("zed://ssh/{host}{path}"),
        Editor::Finder => return None,
    }))
}

/// `text` percent-encoded, letters, digits and `keep` left as they are.
fn encode(text: &str, keep: &[u8]) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || keep.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Open `what`; with `SLOPTY_E2E_OPEN_LOG` set, write it to that file instead.
pub(crate) fn launch(what: &Open, cx: &mut App) {
    if let Some(log) = std::env::var_os("SLOPTY_E2E_OPEN_LOG") {
        use std::io::Write as _;
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .and_then(|mut file| writeln!(file, "{}", what.said()));
        if let Err(error) = written {
            tracing::warn!(%error, "could not log an open");
        }
        return;
    }
    match what {
        Open::Url(url) => cx.open_url(url),
        Open::App { app, path } => {
            let spawned = std::process::Command::new("open").args(["-a", app, path]).spawn();
            if let Err(error) = spawned {
                crate::toast::error_with_detail(
                    cx,
                    format!("Couldn\u{2019}t open {app}. Check that it is installed, then try again."),
                    error.to_string(),
                );
            }
        }
    }
}

/// The Open in menu, at where it was asked for.
#[derive(Clone)]
pub(super) struct OpenInMenu {
    pub chat_id: String,
    pub position: Point<Pixels>,
}

impl Shell {
    /// Whether this build offers Open in: a Mac's, where the editors are.
    pub(super) fn offers_open_in() -> bool {
        cfg!(target_os = "macos")
    }

    /// The folder chat `chat_id` works in, and the address of its machine when that is not this
    /// Mac. slopty: where it works, else the folder its agent's session runs in, else its
    /// project's; a worker reached over loopback is this Mac, and a folder here is whole (`~`
    /// spelled out), as `open -a` needs it.
    fn chat_folder(&self, chat_id: &str, cx: &App) -> Option<(String, Option<String>)> {
        let state = self.state.read(cx);
        let chat = state.chats.iter().find(|chat| chat.id == chat_id)?;
        let path = [chat.cwd.as_ref(), chat.harness_session_cwd.as_ref()]
            .into_iter()
            .flatten()
            .find(|cwd| !cwd.is_empty())
            .cloned()
            .or_else(|| state.space_for_chat(chat).map(|space| space.path.clone()))?;
        let host = state
            .devices
            .iter()
            .find(|device| device.id == chat.device_id)
            .and_then(|device| device.ssh_host.clone())
            .filter(|host| !host.is_empty() && !on_this_machine(host));
        let path = match host {
            Some(_) => path,
            None => expand_home(&path, std::env::var("HOME").ok().as_deref()),
        };
        Some((path, host))
    }

    /// What Open in offers for chat `chat_id`: each editor here that can open its folder.
    pub(super) fn open_in_choices(&self, chat_id: &str, cx: &App) -> Vec<(Editor, Open)> {
        let Some((path, host)) = self.chat_folder(chat_id, cx) else { return Vec::new() };
        Editor::ALL
            .into_iter()
            .filter(|editor| editor.installed())
            .filter_map(|editor| Some((editor, open(editor, &path, host.as_deref())?)))
            .collect()
    }

    /// Show the Open in menu for chat `chat_id` at `position`.
    pub(super) fn open_open_in(
        &mut self,
        chat_id: String,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        self.open_in.open(OpenInMenu { chat_id, position });
        cx.notify();
    }

    /// ⌥⌘O: the selected chat's Open in menu under its button, or shut when it is up. Whether
    /// there was a chat to do it for.
    pub(super) fn toggle_open_in(&mut self, cx: &mut Context<Self>) -> bool {
        if !Self::offers_open_in() {
            return false;
        }
        if self.open_in.get().is_some() {
            self.close_open_in(cx);
            return true;
        }
        let Some(chat_id) = self.state.read(cx).selected_chat.clone() else { return false };
        let Some(at) = under_button() else { return false };
        self.open_open_in(chat_id, at, cx);
        true
    }

    fn close_open_in(&mut self, cx: &mut Context<Self>) {
        if self.open_in.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.open_in);
        }
        cx.notify();
    }

    /// The Open in menu, while it is up.
    pub(super) fn render_open_in_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let OpenInMenu { chat_id, position } = self.open_in.get().cloned()?;
        let closing = self.open_in.closing_since();
        let theme = Theme::of(cx).for_popup();
        let choices = self.open_in_choices(&chat_id, cx);
        let mut list = div().flex().flex_col().gap(px(2.0)).child(popover::menu_heading(&theme, "Open in"));
        if choices.is_empty() {
            list = list.child(
                div()
                    .id("open-in-none")
                    .role(gpui::Role::Label)
                    .aria_label("No editor here can open it")
                    .px(px(8.0))
                    .py(px(6.0))
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text_muted)
                    .child("No editor here can open it"),
            );
        }
        for (editor, what) in choices {
            let glyph = match editor {
                Editor::Finder => icons::FOLDER,
                _ => icons::FILE_CODE,
            };
            list = list.child(
                popover::menu_row(&theme, false, format!("open-in-{}", editor.app()))
                    .id(SharedString::from(format!("open-in-{}", editor.app())))
                    .role(gpui::Role::MenuItem)
                    .aria_label(SharedString::from(editor.app()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        launch(&what, cx);
                        this.close_open_in(cx);
                    }))
                    .child(icon(glyph).size(px(16.0)).text_color(theme.text_muted))
                    .child(SharedString::from(editor.app())),
            );
        }
        // slopty: the menu is a node of its own, its rows its items.
        let card = popover::popover_card(&theme)
            .id("open-in-card")
            .role(gpui::Role::Menu)
            .aria_label("Open in")
            .w(px(216.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_open_in(cx)))
            .child(list)
            .into_any_element();
        Some(popover::menu_at("open-in-menu", position, card, closing))
    }

    /// The chat titlebar's Open in button; `None` off a Mac or with no chat shown.
    pub(super) fn render_open_in_button(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !Self::offers_open_in() {
            return None;
        }
        let chat_id = self.state.read(cx).selected_chat.clone()?;
        let theme = Theme::of(cx).clone();
        let open = self.open_in.get().is_some();
        let button = super::header_icon_button(
            "open-in",
            icons::FILE_CODE,
            open,
            &theme,
            cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if this.open_in.take_press_was_open() {
                    this.close_open_in(cx);
                    return;
                }
                // slopty: under the button, whether pressed or clicked, never at the pointer.
                let at = under_button().unwrap_or_else(|| event.position());
                this.open_open_in(chat_id.clone(), at, cx);
            }),
        )
        .role(gpui::Role::Button);
        // slopty: its tooltip names the key, and goes while the menu is up rather than cover it.
        let button = if open {
            button.aria_label("Open in…")
        } else {
            button.action_hint("Open in…", Keys::Action(Box::new(super::chrome_keys::OpenIn)), cx)
        };
        Some(
            div()
                .flex_none()
                .on_children_prepainted(|bounds, _, _| {
                    BUTTON.with(|button| button.set(bounds.first().copied()));
                })
                .child(button
            .flex_none()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.open_in.note_trigger_press()),
            )
            )
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_here_opens_with_the_app() {
        assert_eq!(
            open(Editor::Zed, "/Users/me/my app", None),
            Some(Open::App { app: "Zed", path: "/Users/me/my app".into() })
        );
        assert_eq!(
            open(Editor::Finder, "/Users/me", Some("")),
            Some(Open::App { app: "Finder", path: "/Users/me".into() }),
            "an empty address is this Mac"
        );
    }

    /// A folder on another machine opens through each editor's SSH remote, its path encoded.
    #[test]
    fn a_folder_on_a_worker_opens_over_ssh() {
        let host = Some("studio.tail1234.ts.net");
        let path = "/home/me/my app/café";
        assert_eq!(
            open(Editor::VsCode, path, host),
            Some(Open::Url(
                "vscode://vscode-remote/ssh-remote+studio.tail1234.ts.net/home/me/my%20app/caf%C3%A9"
                    .into()
            ))
        );
        assert_eq!(
            open(Editor::Cursor, path, host),
            Some(Open::Url(
                "cursor://vscode-remote/ssh-remote+studio.tail1234.ts.net/home/me/my%20app/caf%C3%A9"
                    .into()
            ))
        );
        assert_eq!(
            open(Editor::Zed, "/srv/x", Some("me@100.64.0.9")),
            Some(Open::Url("zed://ssh/me@100.64.0.9/srv/x".into()))
        );
        assert_eq!(open(Editor::Finder, path, host), None, "Finder sees only this Mac");
        assert_eq!(open(Editor::VsCode, "~/code", host), None, "a remote path must be whole");
    }

    /// A worker reached over loopback is this Mac: its folders open here, `~` spelled out.
    #[test]
    fn a_loopback_worker_is_this_mac() {
        assert!(on_this_machine("127.0.0.1"));
        assert!(on_this_machine("me@localhost"));
        assert!(on_this_machine("[::1]"));
        assert!(!on_this_machine("studio.tail1234.ts.net"));
        assert!(!on_this_machine("100.64.0.9"));
        assert_eq!(expand_home("~/code/x", Some("/Users/me/")), "/Users/me/code/x");
        assert_eq!(expand_home("~", Some("/Users/me")), "/Users/me");
        assert_eq!(expand_home("~other/x", Some("/Users/me")), "~other/x");
        assert_eq!(expand_home("/srv/x", Some("/Users/me")), "/srv/x");
    }

    #[test]
    fn what_a_run_reads_back() {
        assert_eq!(
            Open::App { app: "Zed", path: "/a b".into() }.said(),
            "open -a Zed /a b"
        );
        assert_eq!(Open::Url("zed://ssh/h/p".into()).said(), "zed://ssh/h/p");
    }
}
