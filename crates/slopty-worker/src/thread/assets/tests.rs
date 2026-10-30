use std::collections::BTreeMap;

use slopty_core::WallMs;
use slopty_proto::thread::wire::Expanded;
use slopty_proto::thread::{
    Clipped, ContentRef, Item, ItemBody, ItemId, ThreadState, ToolCall, ToolDetail, ToolState,
    TurnId, kind,
};
use slopty_proto::visual::{PageDetail, PageSource, VisualDetail};

use super::{MAX_ASSET_BYTES, read};

fn state_with(id: &str, detail: ToolDetail) -> ThreadState {
    let mut state = slopty_agent::claude::history::replay(&[]);
    let call = ToolCall {
        name: "mcp__slopty__show".to_owned(),
        kind: kind::MCP.to_owned(),
        title: "show".to_owned(),
        input: Clipped::whole("{}"),
        state: ToolState::Completed,
        output: None,
        images: Vec::new(),
        detail: Some(detail),
        child: None,
        ended_ms: None,
        seen: None,
        raw: Vec::new(),
    };
    state.items.push(Item {
        id: ItemId(id.to_owned()),
        turn: TurnId(1),
        at_ms: WallMs::ZERO,
        body: ItemBody::Tool(Box::new(call)),
    });
    state
}

fn visual(assets: &[(&str, &std::path::Path)]) -> ToolDetail {
    ToolDetail::Visual(VisualDetail {
        title: "chart".to_owned(),
        script: Clipped::whole("draw()"),
        assets: assets
            .iter()
            .map(|(name, path)| ((*name).to_owned(), path.display().to_string()))
            .collect::<BTreeMap<_, _>>(),
    })
}

fn page(file: &std::path::Path) -> ToolDetail {
    ToolDetail::Page(PageDetail {
        title: "report".to_owned(),
        source: PageSource::File(file.display().to_string()),
    })
}

#[tokio::test]
async fn a_visuals_asset_is_read_by_its_name() {
    let dir = tempfile::tempdir().unwrap();
    let logo = dir.path().join("logo.png");
    std::fs::write(&logo, b"\x89PNG-bytes").unwrap();
    let state = state_with("toolu_1", visual(&[("logo", &logo)]));
    let body = read(&state, &ContentRef::item_asset("toolu_1", "logo")).await;
    assert_eq!(body, Expanded::Bytes(b"\x89PNG-bytes".to_vec()), "the asset's bytes");
}

#[tokio::test]
async fn a_pages_sibling_is_read_under_its_folder() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("css")).unwrap();
    let index = dir.path().join("index.html");
    std::fs::write(&index, "<link href=css/site.css>").unwrap();
    std::fs::write(dir.path().join("css/site.css"), "body{}").unwrap();
    let state = state_with("toolu_2", page(&index));
    let body = read(&state, &ContentRef::item_asset("toolu_2", "css/site.css")).await;
    assert_eq!(body, Expanded::Bytes(b"body{}".to_vec()), "the sibling's bytes");
}

#[tokio::test]
async fn a_path_out_of_the_pages_folder_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    std::fs::create_dir_all(&site).unwrap();
    let index = site.join("index.html");
    std::fs::write(&index, "<p>").unwrap();
    std::fs::write(dir.path().join("secret.txt"), "no").unwrap();
    std::os::unix::fs::symlink(dir.path().join("secret.txt"), site.join("link.txt")).unwrap();
    let state = state_with("toolu_3", page(&index));
    let outside = dir.path().join("secret.txt").display().to_string();
    for name in ["../secret.txt", "./../secret.txt", outside.as_str(), "link.txt", ""] {
        let body = read(&state, &ContentRef::item_asset("toolu_3", name)).await;
        assert_eq!(body, Expanded::Gone, "{name:?} is refused");
    }
}

#[tokio::test]
async fn a_file_over_the_cap_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let (fits, big) = (dir.path().join("fits.bin"), dir.path().join("big.bin"));
    let cap = usize::try_from(MAX_ASSET_BYTES).unwrap();
    std::fs::write(&fits, vec![7_u8; cap]).unwrap();
    std::fs::write(&big, vec![7_u8; cap + 1]).unwrap();
    let state = state_with("toolu_4", visual(&[("fits", &fits), ("big", &big)]));
    let read_len = |body: Expanded| match body {
        Expanded::Bytes(bytes) => Some(bytes.len()),
        Expanded::Text(_) | Expanded::Gone => None,
    };
    let at_cap = read(&state, &ContentRef::item_asset("toolu_4", "fits")).await;
    assert_eq!(read_len(at_cap), Some(cap), "a file at the cap is read whole");
    let over = read(&state, &ContentRef::item_asset("toolu_4", "big")).await;
    assert_eq!(over, Expanded::Gone, "a file over the cap is gone");
}

#[tokio::test]
async fn an_unknown_name_or_item_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let logo = dir.path().join("logo.png");
    std::fs::write(&logo, b"png").unwrap();
    let state = state_with("toolu_5", visual(&[("logo", &logo)]));
    for content in [
        ContentRef::item_asset("toolu_5", "icon"),
        ContentRef::item_asset("toolu_9", "logo"),
        ContentRef("toolu_5#logo".to_owned()),
    ] {
        assert_eq!(read(&state, &content).await, Expanded::Gone, "{content:?} names nothing");
    }
    std::fs::remove_file(&logo).unwrap();
    let missing = read(&state, &ContentRef::item_asset("toolu_5", "logo")).await;
    assert_eq!(missing, Expanded::Gone, "a missing file is gone");
}
