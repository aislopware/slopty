//! What "Open in `<editor>`" needs from the app: the person's editor link from the settings,
//! and what the app knows of each machine (its SSH name, its home, its place in Finder,
//! whether it is this Mac), told to the tiles' global ([`slopty_ui::file::open_with`]).

use gpui::{App, Context};
use slopty_ui::file::open_with::{self, Machine, OpenInEditor};
use slopty_ui::palette::PaletteItem;

use crate::Workspace;
use crate::ssh::Target;
use crate::workers::worker_key;

/// The palette's line for the focused tile, named for the editor ("Open in Zed"); none where
/// nothing would open. A tile offers its action only while it would open its path, so the line
/// shows on file, folder and review tiles alone.
pub(crate) fn palette_line(cx: &App) -> Option<PaletteItem> {
    let label = match cx.try_global::<open_with::Editors>() {
        Some(editors) => editors.label()?,
        None => open_with::Editors::default().label()?,
    };
    let icon = slopty_ui::icons::IconName::ExternalLink;
    Some(PaletteItem::new(&label, icon, Box::new(OpenInEditor), &[]))
}

/// How SSH reaches `to`, as an editor's remote link names it: `[user@]host[:port]`.
pub(crate) fn ssh_name(to: &Target) -> String {
    let user = to.user.as_ref().map(|user| format!("{user}@")).unwrap_or_default();
    let host = match to.port {
        Some(port) if to.host.contains(':') => format!("[{}]:{port}", to.host),
        Some(port) => format!("{}:{port}", to.host),
        None => to.host.clone(),
    };
    format!("{user}{host}")
}

impl Workspace {
    /// Tell the tiles what the directory lists of each machine: its name, how SSH reaches it
    /// when it was installed from here, its place in Finder, and whether it is this Mac. Its
    /// home comes with its link ([`tell_editor_home`]).
    pub(crate) fn tell_editor_machines(&self, cx: &mut Context<Self>) {
        let listed: Vec<_> = self
            .directory
            .workers()
            .map(|info| {
                let ssh = self.deployer.as_ref().and_then(|d| d.target_of(info.worker));
                (info.worker, info.name.clone(), ssh.as_ref().map(ssh_name))
            })
            .collect();
        #[cfg(target_os = "macos")]
        let shared = slopty_platform::files::container();
        for (id, name, ssh) in listed {
            #[cfg(target_os = "macos")]
            let finder = shared.as_deref().and_then(|dir| slopty_platform::files::root(dir, id));
            #[cfg(not(target_os = "macos"))]
            let finder = None;
            let here = self.this_mac_worker == Some(id);
            open_with::update_machine(
                worker_key(id),
                |m| {
                    m.name = name;
                    m.ssh = ssh;
                    m.finder = finder;
                    m.here = here;
                },
                cx,
            );
        }
    }
}

/// The link to `key` is up: its name and home, as it said them.
pub(crate) fn tell_editor_home(
    key: slopty_client::layout::WorkerKey,
    name: &str,
    home: &str,
    cx: &mut App,
) {
    open_with::update_machine(
        key,
        |m: &mut Machine| {
            name.clone_into(&mut m.name);
            m.home = (!home.is_empty()).then(|| home.to_owned());
        },
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The palette's line names the editor the link opens, and goes on a phone with no link.
    #[gpui::test]
    fn the_palette_names_the_editor(cx: &gpui::TestAppContext) {
        cx.update(|cx| {
            let none = palette_line(cx).map(|line| line.label);
            let mac = cfg!(target_os = "macos");
            assert_eq!(none.as_deref(), mac.then_some(open_with::OPEN_WITH_DEFAULT));
            let file = "[client]\neditor = \"zed://ssh/{host}{path}\"\n";
            let link = slopty_settings::Settings::parse(file).settings.client.editor;
            open_with::set_link(link, cx);
            assert_eq!(palette_line(cx).map(|line| line.label).as_deref(), Some("Open in Zed"));
        });
    }

    /// A machine installed from here is named as SSH reached it, user and port included, an
    /// IPv6 address bracketed before its port.
    #[test]
    fn the_ssh_name_is_the_target_s() {
        assert_eq!(ssh_name(&Target::host("studio")), "studio");
        let full = Target {
            host: "studio.tail".to_owned(),
            user: Some("me".to_owned()),
            port: Some(2222),
        };
        assert_eq!(ssh_name(&full), "me@studio.tail:2222");
        let v6 = Target { host: "fd7a::1".to_owned(), user: None, port: Some(22) };
        assert_eq!(ssh_name(&v6), "[fd7a::1]:22");
    }
}
