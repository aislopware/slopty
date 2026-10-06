//! What the layout keeps beside its tiling: worker keys, the navigator, the window's frame.
//! The tiling itself is tested in [`super::tiling`].

use std::str::FromStr as _;

use super::*;

fn tile(worker: u128, n: u128) -> TileRef {
    let item = ItemId::from_str(&format!("00000000-0000-0000-0000-{n:012x}")).unwrap();
    TileRef { worker: WorkerKey::new(worker), item }
}

#[test]
fn a_worker_key_prints_and_serialises_as_32_hex_digits() {
    let key = WorkerKey::new(0xab);
    assert_eq!(key.to_string(), format!("{:032x}", 0xab));
    assert_eq!(format!("{key:?}"), format!("WorkerKey({key})"));
    let json = serde_json::to_string(&key).unwrap();
    assert_eq!(json, format!("\"{key}\""));
    assert_eq!(serde_json::from_str::<WorkerKey>(&json).unwrap(), key);
    assert!(serde_json::from_str::<WorkerKey>("\"not hex\"").is_err(), "garbage is refused");
    // The first 16 bytes, big-endian; short input zero-padded at the end.
    let long: Vec<u8> = (1..=20).collect();
    assert_eq!(
        WorkerKey::from_bytes(&long).value(),
        u128::from_be_bytes(long[..16].try_into().unwrap())
    );
    assert_eq!(WorkerKey::from_bytes(&[0xff]).value(), 0xff_u128 << 120);
    assert_eq!(WorkerKey::from_bytes(&[]).value(), 0);
    // A tile ref round-trips.
    let r = tile(7, 9);
    let back: TileRef = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back, r);
}

/// The navigator's grouping and the projects' tiling are kept together in `layout.json`, and
/// come back as they were.
#[test]
fn the_tiling_and_the_navigator_are_saved_and_restored() {
    let mut tiling = Tiling::new(TilingConfig::default());
    tiling.set_area(1264.0, 800.0);
    tiling.new_tab(tile(1, 1), &GroupKey::new("project", "atlas"));
    tiling.split_focused(tile(1, 2), Side::Right, &GroupKey::new("project", "atlas"));
    let by_agent = vec!["agent".to_owned(), "machine".to_owned()];
    let saved = Saved {
        tiling: tiling.save(),
        navigator: Navigator { group_by: by_agent.clone(), ..Navigator::default() },
        ..Saved::default()
    };
    let json = serde_json::to_string(&saved).unwrap();
    let back: Saved = serde_json::from_str(&json).unwrap();
    assert_eq!(back.navigator.group_by, by_agent);
    let restored = Tiling::restore(back.tiling, TilingConfig::default());
    assert_eq!(restored.save(), tiling.save());
    assert_eq!(Navigator::default().group_by, ["project", "repo", "folder", "machine"]);
}

/// A window's frame is kept to open it there again only when it can be: finite, and not a
/// sliver narrower or shorter than [`WindowFrame::MIN`].
#[test]
fn a_window_frame_is_kept_only_when_it_can_be_opened_again() {
    let frame = WindowFrame {
        display: Some("37D8832A-2D66-02CA-B9F7-8F30A301B230".to_owned()),
        x: 40.0,
        y: 30.0,
        width: 1280.0,
        height: 800.0,
        fullscreen: false,
    };
    assert!(frame.sane());
    for bad in [
        WindowFrame { x: f32::NAN, ..frame.clone() },
        WindowFrame { width: f32::INFINITY, ..frame.clone() },
        WindowFrame { height: WindowFrame::MIN - 1.0, ..frame.clone() },
    ] {
        assert!(!bad.sane(), "{bad:?}");
    }
    let saved = Saved { window: Some(frame.clone()), ..Saved::default() };
    let json = serde_json::to_string(&saved).expect("serialises");
    let back: Saved = serde_json::from_str(&json).expect("parses");
    assert_eq!(back.window, Some(frame), "round-trips through layout.json");
}
