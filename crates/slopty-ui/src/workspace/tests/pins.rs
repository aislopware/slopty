//! A tile put in a project by hand.

use slopty_client::groups::{GroupKey, fact};

use super::super::actions::PinToProject;
use super::palette::shell_in;
use super::*;

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
