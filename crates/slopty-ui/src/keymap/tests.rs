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
    assert_eq!(chords_of(&keymap, Scope::Workspace, "new_terminal"), ["cmd-t", "cmd-n"]);
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
    assert_eq!(chords_of(&keymap, Scope::Workspace, "new_terminal"), ["cmd-n"], "⌘T went");
    assert!(chords_of(&keymap, Scope::Workspace, "close_tile").is_empty(), "unbound");
    let close = keymap.bindings(|_| true);
    assert!(!close.iter().any(|b| b.action().partial_eq(&ws::CloseItem)), "no ⌘W at all");
    assert_eq!(chords_of(&keymap, Scope::Workspace, "new_agent"), ["cmd-shift-i"]);
    assert!(chords_of(&keymap, Scope::Workspace, "toggle_mute").is_empty(), "the later lost");
    assert!(chords_of(&keymap, Scope::Workspace, "toggle_stats").is_empty(), "the default lost");
    let said = keymap.diagnostics();
    assert!(
        said.contains(
            &"⌘T runs `workspace.new_note` now, no longer `workspace.new_terminal`".to_owned()
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
    assert_eq!(chords_of(&keymap, Scope::Workspace, "find_everywhere"), ["cmd-shift-f"]);
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
    assert!(current().is_set(current().find(Scope::Workspace, "new_note").unwrap_or_default()));

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
        crate::workspace::palette_items()
            .into_iter()
            .find(|item| item.label == label)
            .map_or_else(|| panic!("{label}"), |item| item.keys)
    };
    assert_eq!(keys_of("New terminal"), "⌘T");
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
        ["⌘K runs `workspace.new_terminal` now, no longer `terminal.clear_screen`"]
    );

    let app = || vec![app_command("open_settings", ws::OpenPalette, &["cmd-,"])];
    let keymap = Keymap::new(&keys("[keys.app]\nopen_settings = \"cmd-f\"\n"), app());
    for scope in [Scope::Workspace, Scope::Terminal, Scope::File, Scope::Conversation] {
        assert!(chords_of(&keymap, scope, "find").is_empty(), "{scope:?} gave ⌘F up");
    }
    assert_eq!(keymap.diagnostics().len(), 4, "{:?}", keymap.diagnostics());

    let keymap = Keymap::new(&keys("[keys.terminal]\nfind = \"cmd-t\"\n"), app());
    assert!(keymap.diagnostics().is_empty(), "the file's deeper chord takes nothing around it");
    assert_eq!(chords_of(&keymap, Scope::Workspace, "new_terminal"), ["cmd-t", "cmd-n"]);
}

/// The palette's words for an action's chord are the keymap's, worked out as it is made: the
/// first command that has a chord for it, after the file's.
#[test]
fn a_chords_words_come_with_the_keymap() {
    let keymap = Keymap::default();
    assert_eq!(keymap.label_of(&ws::NewTerminal), "⌘T");
    assert_eq!(keymap.label_of(&ws::OpenFile), "", "no chord");
    let keymap = Keymap::new(
        &keys("[keys.workspace]\nnew_terminal = \"cmd-alt-y\"\nnew_agent = \"\"\n"),
        Vec::new(),
    );
    assert_eq!(keymap.label_of(&ws::NewTerminal), "⌥⌘Y");
    assert_eq!(keymap.label_of(&ws::NewAgent), "");
}
