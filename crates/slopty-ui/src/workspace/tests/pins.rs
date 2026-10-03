//! A tile put in a project by hand, a project named from the palette, and a declared project's
//! members claiming the tiles they name.

use slopty_client::groups::{GroupKey, fact};
use slopty_proto::orchestration::{Outcome, Verb};
use slopty_proto::project::Matcher;

use super::super::actions::{NameProject, PinToProject};
use super::super::project_lines::NAME_PROJECT;
use super::palette::shell_in;
use super::*;
use crate::project::fixtures;

fn project_of(view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef) -> GroupKey {
    view.read_with(cx, |v, _| v.project_groups().group_of(tile).map(|g| g.key.clone()))
        .expect("a group")
}

fn labels(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |v, _| v.pin_lines().into_iter().map(|l| l.label).collect())
}

/// "Add to atlas" on the focused site shell says the pin to its worker as the item's fact, and
/// the shell joins atlas's group at once; "Take out of atlas" says it back and the shell is
/// site's again.
#[gpui::test]
fn adding_a_tile_to_a_project_pins_it_there(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (_, atlas) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    let (_, site) = shell_in(&view, cx, &studio, 2, "/w/site", true);
    let (atlas_key, site_key) = (project_of(&view, cx, atlas), project_of(&view, cx, site));
    view.update_in(cx, |v, _w, cx| v.focus_tile(site, cx));
    cx.run_until_parked();
    studio.drain();
    let lines = labels(&view, cx);
    assert!(lines.contains(&"Add to atlas".to_owned()), "{lines:?}");
    assert!(!lines.contains(&"Add to site".to_owned()), "not to its own: {lines:?}");

    cx.dispatch_action(PinToProject { project: Some(atlas_key.clone()) });
    cx.run_until_parked();
    let said = ItemOp::SetFact {
        id: site.item,
        key: fact::PROJECT.to_owned(),
        value: Some(atlas_key.as_str().to_owned()),
    };
    assert_eq!(studio.drain(), [ClientMsg::Items(said)]);
    assert_eq!(project_of(&view, cx, site), atlas_key, "it joins atlas");
    assert!(labels(&view, cx).contains(&"Take out of atlas".to_owned()));

    cx.dispatch_action(PinToProject { project: None });
    cx.run_until_parked();
    let unsaid = ItemOp::SetFact { id: site.item, key: fact::PROJECT.to_owned(), value: None };
    assert_eq!(studio.drain(), [ClientMsg::Items(unsaid)]);
    assert_eq!(project_of(&view, cx, site), site_key, "and is site's again");
}

/// "Name this project…" puts the project's name in the focused tile's header; ↩ keeps it on
/// the server under the name typed, its members the place its tiles are in, spelled with the
/// machine's name.
#[gpui::test]
fn naming_a_project_keeps_it_on_the_server_with_its_members(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (_, atlas) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.focus_tile(atlas, cx);
    });
    cx.run_until_parked();
    assert!(labels(&view, cx).contains(&NAME_PROJECT.to_owned()));

    cx.dispatch_action(NameProject);
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("project-name", atlas.item)).is_some(), "the field is up");
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("Atlas app");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let mut verbs = Vec::new();
    while let Some((verb, reply)) = queue.try_next() {
        let _gone = reply.send(Outcome::Done);
        verbs.push(verb);
    }
    let [Verb::ProjectCreate { project, title, members, orchestrator: None, .. }] =
        verbs.as_slice()
    else {
        panic!("a project: {verbs:?}");
    };
    assert_eq!((project.as_str(), title.as_str()), ("atlas-app", "Atlas app"));
    let place: Matcher = [
        (fact::MACHINE.to_owned(), "studio".to_owned()),
        (fact::CWD.to_owned(), "/w/atlas".to_owned()),
    ]
    .into();
    assert_eq!(members, &[place]);
}

/// A declared project's members claim the tiles they name, on the machine named, and only
/// there: the notes shell on the studio joins it, the laptop's at the same path does not.
#[gpui::test]
fn a_projects_members_claim_the_tiles_they_name(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let here = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/w/notes/a"));
    let there = opens_in(&view, cx, &laptop, SessionId::new(), laptop.me, 1, Some("/w/notes/a"));
    let mut notes = fixtures::project("notes", None);
    notes.members = vec![
        [
            (fact::MACHINE.to_owned(), "studio".to_owned()),
            (fact::CWD.to_owned(), "/w/notes".to_owned()),
        ]
        .into(),
    ];
    view.update_in(cx, |v, _w, cx| {
        v.projects_part(fixtures::snapshot(1, vec![fixtures::status(notes, vec![], vec![])]), cx);
    });
    cx.run_until_parked();
    assert_eq!(project_of(&view, cx, here), GroupKey::new(fact::PROJECT, "notes"));
    assert_ne!(project_of(&view, cx, there), GroupKey::new(fact::PROJECT, "notes"));
}
