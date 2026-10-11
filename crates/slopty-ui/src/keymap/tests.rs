use gpui::{Keystroke, TestAppContext};
use slopty_settings::Settings;

use super::*;

/// `[keys]` from a file's text.
fn keys(text: &str) -> KeySettings {
    let loaded = Settings::parse(text);
    assert!(loaded.error.is_none(), "{loaded:?}");
    loaded.settings.keys
}

fn chords_of(keymap: &Keymap, scope: Scope, name: &str) -> Vec<String> {
    let ix = keymap.find(scope, name).unwrap_or_else(|| panic!("{name}"));
    keymap.chords(ix).to_vec()
}

/// Every default reads in the palette's syntax and binds the keystroke it was written as;
/// every name is one command in its scope, spelled as the file spells it; every context parses;
/// and no two defaults want one chord in one context.
#[test]
fn the_table_reads_and_holds_no_clash() {
    let keymap = Keymap::default();
    assert!(keymap.diagnostics().is_empty(), "{:?}", keymap.diagnostics());
    let mut seen = std::collections::HashSet::new();
    for (ix, command) in keymap.commands().iter().enumerate() {
        assert!(seen.insert(command.key()), "{} twice", command.key());
        let lower = command
            .name()
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit());
        assert!(lower, "{}", command.key());
        assert_eq!(keymap.chords(ix).len(), command.defaults.len(), "{}", command.key());
        for (written, read) in command.defaults.iter().zip(keymap.chords(ix)) {
            assert_eq!(
                Keystroke::parse(read).ok(),
                Keystroke::parse(written).ok(),
                "{}: {written}",
                command.key()
            );
        }
    }
    let bound = keymap.bindings(|_| true);
    let expected: usize = keymap
        .commands()
        .iter()
        .enumerate()
        .map(|(ix, c)| keymap.chords(ix).len() * c.contexts.len())
        .sum();
    assert_eq!(bound.len(), expected, "every chord in every context, none dropped");
    assert!(bound.iter().all(|b| b.meta() == Some(OURS)));
}

/// What the file names and the keymap does not know is said, one line each, and the rest of
/// the file still applies: an unknown context, an unknown action, a chord that names no key.
#[test]
fn the_file_is_told_what_it_named_wrong() {
    let keymap = Keymap::new(
        &keys(
            "[keys.editor]\nsave = \"cmd-s\"\n[keys.workspace]\nnew_portal = \"cmd-t\"\nnew_note = [\"cmd-wat\", \"cmd-alt-n\"]\n",
        ),
        Vec::new(),
    );
    let said = keymap.diagnostics();
    assert_eq!(said.len(), 3, "{said:?}");
    assert!(said[0].starts_with("unknown context `keys.editor`"), "{said:?}");
    assert!(said[0].contains("workspace, terminal"), "the contexts there are: {said:?}");
    assert!(said.iter().any(|s| s.contains("`keys.workspace.new_note`: no key is called \"wat\"")));
    assert!(said.contains(&"unknown action `keys.workspace.new_portal`".to_owned()));
    assert_eq!(
        chords_of(&keymap, Scope::Workspace, "new_note"),
        ["alt-cmd-n"],
        "the chord that reads"
    );
    assert_eq!(chords_of(&keymap, Scope::Workspace, "new_terminal"), ["cmd-shift-t"]);
}

/// The file's chord replaces the default's, none unbinds, and a chord the file gives one
/// command is taken from a default that held it in the same context, which is said naming
/// both. Two of the file's own on one chord keep the first; a nested context is no clash.
#[test]
fn the_file_overrides_unbinds_and_wins_a_clash() {
    let keymap = Keymap::new(
        &keys(
            "[keys.workspace]\nnew_note = \"cmd-t\"\nclose_tile = \"none\"\nnew_agent = \"cmd-shift-i\"\ntoggle_mute = \"cmd-shift-i\"\n[keys.terminal]\nfind = \"cmd-shift-f\"\n",
        ),
        Vec::new(),
    );
    assert_eq!(chords_of(&keymap, Scope::Workspace, "new_note"), ["cmd-t"]);
    assert!(chords_of(&keymap, Scope::Workspace, "start_agent").is_empty(), "⌘T went");
    assert_eq!(chords_of(&keymap, Scope::Workspace, "new_terminal"), ["cmd-shift-t"]);
    assert!(chords_of(&keymap, Scope::Workspace, "close_tile").is_empty(), "unbound");
    let close = keymap.bindings(|_| true);
    assert!(!close.iter().any(|b| b.action().partial_eq(&ws::CloseItem)), "no ⌘W at all");
    assert_eq!(chords_of(&keymap, Scope::Workspace, "new_agent"), ["cmd-shift-i"]);
    assert!(chords_of(&keymap, Scope::Workspace, "toggle_mute").is_empty(), "the later lost");
    assert!(chords_of(&keymap, Scope::Workspace, "toggle_stats").is_empty(), "the default lost");
    let said = keymap.diagnostics();
    assert!(
        said.contains(
            &"⌘T runs `workspace.new_note` now, no longer `workspace.start_agent`".to_owned()
        ),
        "{said:?}"
    );
    assert!(
        said.contains(
            &"⇧⌘I is set for both `workspace.new_agent` and `workspace.toggle_mute`; `workspace.new_agent` keeps it"
                .to_owned()
        ),
        "{said:?}"
    );
    assert_eq!(said.len(), 3, "and the stats' ⇧⌘I: {said:?}");
    // The workspace's ⇧⌘F binds in a text field too; the terminal is a context of its own.
    assert_eq!(chords_of(&keymap, Scope::Terminal, "find"), ["cmd-shift-f"]);
    assert_eq!(chords_of(&keymap, Scope::Workspace, "search_in_files"), ["cmd-shift-f"]);
    assert!(keymap.is_set(keymap.find(Scope::Terminal, "find").unwrap_or_default()));
    let same = Keymap::new(&KeySettings::default(), Vec::new());
    assert!(same.binds_as(&Keymap::default()) && !keymap.binds_as(&same));
}

/// A new keymap replaces the one bound before in GPUI and nothing else: a text field's own
/// binding stays, ahead of the keymap's, and a chord the last keymap gave is gone.
#[gpui::test]
fn installing_rebinds_in_place(cx: &mut TestAppContext) {
    let foreign = KeyBinding::new("cmd-x", gpui_kit::component::input::MoveUp, Some("Input"));
    cx.update(|cx| cx.bind_keys([foreign]));
    let chord_for = |cx: &mut TestAppContext, action: &dyn Action| {
        cx.update(|cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            keymap
                .bindings_for_action(action)
                .map(|b| b.keystrokes().iter().map(|k| k.inner().unparse()).collect::<String>())
                .collect::<Vec<_>>()
        })
    };
    let changed = Keymap::new(&keys("[keys.workspace]\nnew_note = \"cmd-alt-n\"\n"), Vec::new());
    cx.update(|cx| install(changed, cx));
    assert_eq!(chord_for(cx, &ws::NewNote), ["alt-cmd-n"]);
    let now = cx.update(|cx| current(cx));
    assert!(now.is_set(now.find(Scope::Workspace, "new_note").unwrap_or_default()));

    cx.update(|cx| install(Keymap::default(), cx));
    assert_eq!(chord_for(cx, &ws::NewNote), ["cmd-shift-n"], "the default again, alone");
    assert_eq!(chord_for(cx, &gpui_kit::component::input::MoveUp), ["cmd-x"], "a field's kept");
    let first = cx.update(|cx| cx.key_bindings().borrow().bindings().next().map(KeyBinding::meta));
    assert_eq!(first, Some(None), "the field's binding ahead of the keymap's");
    let ours = cx.update(|cx| {
        cx.key_bindings().borrow().bindings().filter(|b| b.meta() == Some(OURS)).count()
    });
    assert_eq!(ours, Keymap::default().bindings(|_| true).len(), "one keymap bound, not two");
}

/// The palette's lines show the chord in effect: the file's for an action it rebinds, none for
/// one it unbinds, in the workspace and the terminal alike.
#[gpui::test]
fn the_palette_shows_the_chord_in_effect(cx: &TestAppContext) {
    let keys_of = |label: &str| {
        cx.update(|cx| crate::workspace::palette_items(cx))
            .into_iter()
            .find(|item| item.label == label)
            .map_or_else(|| panic!("{label}"), |item| item.keys)
    };
    assert_eq!(keys_of("New terminal"), "⇧⌘T");
    let text = "[keys.workspace]\nnew_terminal = \"cmd-alt-t\"\nclose_tile = \"\"\n[keys.terminal]\ncopy_last_output = \"ctrl-cmd-c\"\n";
    cx.update(|cx| install(Keymap::new(&keys(text), Vec::new()), cx));
    assert_eq!(keys_of("New terminal"), "⌥⌘T");
    assert_eq!(keys_of("Close tile"), "", "unbound");
    assert_eq!(keys_of("Copy last output"), "⌃⌘C");
}

/// The file's chord wins where a default in a context inside the file's would otherwise take
/// it: the workspace's ⌘K over the terminal's own, and the app's chord over every context's.
/// The default gives it up and the app says so; outside the nesting nothing changes.
#[test]
fn the_files_chord_wins_over_a_deeper_default() {
    let keymap = Keymap::new(&keys("[keys.workspace]\nnew_terminal = \"cmd-k\"\n"), Vec::new());
    assert_eq!(chords_of(&keymap, Scope::Workspace, "new_terminal"), ["cmd-k"]);
    assert!(chords_of(&keymap, Scope::Terminal, "clear_screen").is_empty(), "no ⌘K under it");
    assert_eq!(
        keymap.diagnostics(),
        [
            "⌘K runs `workspace.new_terminal` now, no longer `workspace.open_palette`",
            "⌘K runs `workspace.new_terminal` now, no longer `terminal.clear_screen`",
        ]
    );

    let app = || vec![app_command("open_settings", ws::OpenPalette, &["cmd-,"])];
    let keymap = Keymap::new(&keys("[keys.app]\nopen_settings = \"cmd-f\"\n"), app());
    for scope in [Scope::Workspace, Scope::Terminal, Scope::File, Scope::Conversation] {
        assert!(chords_of(&keymap, scope, "find").is_empty(), "{scope:?} gave ⌘F up");
    }
    // The four finds and the navigator's ⌘F for its filter.
    assert_eq!(keymap.diagnostics().len(), 5, "{:?}", keymap.diagnostics());

    let keymap = Keymap::new(&keys("[keys.terminal]\nfind = \"cmd-t\"\n"), app());
    assert!(keymap.diagnostics().is_empty(), "the file's deeper chord takes nothing around it");
    assert_eq!(chords_of(&keymap, Scope::Workspace, "start_agent"), ["cmd-t"]);
}

/// The palette's words for an action's chord are the keymap's, worked out as it is made: the
/// first command that has a chord for it, after the file's.
#[test]
fn a_chords_words_come_with_the_keymap() {
    let keymap = Keymap::default();
    assert_eq!(keymap.label_of(&ws::NewTerminal), "⇧⌘T");
    assert_eq!(keymap.label_of(&ws::StartAgent), "⌘T");
    assert_eq!(keymap.label_of(&ws::NewGoal), "⌘N", "a goal, the usual start");
    assert_eq!(keymap.label_of(&ws::OpenFile), "⌘P", "quick open");
    assert_eq!(keymap.label_of(&ws::OpenPalette), "⌘K", "search everything");
    assert_eq!(keymap.label_of(&ws::OpenCommands), "⇧⌘P", "the commands");
    assert_eq!(keymap.label_of(&ws::OpenFolder), "", "no chord");
    let keymap = Keymap::new(
        &keys("[keys.workspace]\nnew_terminal = \"cmd-alt-y\"\nnew_agent = \"\"\n"),
        Vec::new(),
    );
    assert_eq!(keymap.label_of(&ws::NewTerminal), "⌥⌘Y");
    assert_eq!(keymap.label_of(&ws::NewAgent), "");
}

/// A key alone is a chord where nothing takes typing: a folder's rows, a board, a PDF's pages
/// in a file tile; not the file tile's editor, whose scope the pages share.
#[test]
fn a_bare_key_binds_where_nothing_takes_typing() {
    let all = defaults();
    let bare = |scope: Scope, name: &str| {
        all.iter().find(|c| c.scope() == scope && c.name() == name).unwrap().takes_bare_keys()
    };
    assert!(bare(Scope::File, "scroll_down"));
    assert!(!bare(Scope::File, "save"), "the editor types");
    assert!(!bare(Scope::File, "close_find"), "a find bar types");
    assert!(bare(Scope::Folder, "open"));
    assert!(!bare(Scope::Workspace, "new_terminal"));
}

/// The whole default table, one command a line in the table's order: its key, the action it
/// runs, its chords as GPUI reads them and the contexts it binds in (`*` everywhere). What
/// another crate declares (the terminal's, a board's, a review's) is held here as the table's
/// own, so moving a command's row between crates cannot rename, rebind or reorder it unseen.
/// The record is `keymap/defaults.txt`; a change to the table is made there too, on purpose.
#[test]
fn the_default_table_is_as_recorded() {
    let keymap = Keymap::default();
    let lines: Vec<String> = keymap
        .commands()
        .iter()
        .enumerate()
        .map(|(ix, command)| {
            let contexts: Vec<&str> =
                command.contexts.iter().map(|context| context.unwrap_or("*")).collect();
            format!(
                "{} {} [{}] {}",
                command.key(),
                command.action().name(),
                keymap.chords(ix).join(", "),
                contexts.join(" | ")
            )
        })
        .collect();
    let recorded: Vec<&str> = include_str!("defaults.txt").lines().collect();
    let table = lines.join("\n");
    for (ix, (now, then)) in lines.iter().zip(&recorded).enumerate() {
        assert_eq!(now, then, "line {} of `keymap/defaults.txt`; the table now:\n{table}", ix + 1);
    }
    assert_eq!(lines.len(), recorded.len(), "the table now:\n{table}");
}

/// Under gpui-kit's window root, the keys the kit binds around every view without ⌘ are a
/// shell's once the keymap is installed (`released`): Tab and ⇧Tab type rather than walk the
/// focus, and ⌃C, which the kit binds to copy off Apple's platforms (bound here as it binds it
/// there), interrupts rather than copies. The shell keeps the focus.
#[gpui::test]
fn the_kits_keys_around_a_shell_go_to_the_program(cx: &mut TestAppContext) {
    use gpui::{AppContext as _, Focusable as _, px, size};
    use slopty_core::SessionId;
    use slopty_proto::ClientMsg;
    use slopty_proto::input::{KeyCode, Mods};
    use slopty_proto::terminal::{TermRequest, TermSize};
    use slopty_terminal::TerminalView;
    use tokio::sync::mpsc;

    let (tx, mut rx) = mpsc::channel(64);
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys([KeyBinding::new("ctrl-c", gpui_kit::component::input::Copy, Some("Root"))]);
        install(Keymap::default(), cx);
    });
    let grid = TermSize { cols: 10, rows: 3, ..TermSize::default() };
    let theme = slopty_theme::Theme::default();
    let view = cx.new(|cx| TerminalView::new(SessionId::new(), grid, tx, theme, cx));
    let held = view.clone();
    let (_root, cx) = cx.add_window_view(|window, cx| {
        let focus = held.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        gpui_kit::component::Root::new(held, window, cx)
    });
    cx.simulate_resize(size(px(400.0), px(300.0)));
    cx.run_until_parked();
    let keys = |rx: &mut mpsc::Receiver<ClientMsg>| {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|msg| match msg {
                ClientMsg::Term { req: TermRequest::Key(key), .. } => Some((key.code, key.mods)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    keys(&mut rx);
    cx.simulate_keystrokes("tab shift-tab ctrl-c");
    cx.run_until_parked();
    assert_eq!(
        keys(&mut rx),
        [(KeyCode::Tab, Mods::empty()), (KeyCode::Tab, Mods::SHIFT), (KeyCode::C, Mods::CTRL)]
    );
    let focused = cx.update(|window, cx| view.read(cx).focus_handle(cx).is_focused(window));
    assert!(focused, "the shell kept the focus");
}
