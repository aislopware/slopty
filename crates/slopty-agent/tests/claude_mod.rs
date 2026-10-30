//! Slopty's Claude Code mod as a real Claude Code ran it (`tests/fixtures/claude-mod`, written by
//! `cargo xtask fixtures claude-mod`): what it relayed as the recorder typed, pasted, recalled and
//! queued in the TUI, the transcript's queue records of the same run, and where Claude Code stored
//! the pasted picture. The embedded module and the version are held to the recording, and each
//! relay is read through the hook and the box as the worker reads it.

#[cfg(test)]
mod claude_mod {
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use serde_json::Value;
    use slopty_agent::claude::draft::{self, Pictures};
    use slopty_agent::claude::sessions;
    use slopty_agent::conversation::{Change, Conversation};
    use slopty_agent::hooks::{DRAFT_EVENT, Hook, Signal};
    use slopty_agent::transcript::Tail;
    use slopty_core::WallMs;

    /// A box as a test sees it: its words, and each picture's number and media type.
    type Shown = (String, Vec<(u32, String)>);

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-mod").join(name)
    }

    fn read(name: &str) -> String {
        std::fs::read_to_string(fixture(name)).expect("a fixture")
    }

    /// The module embedded is the one recorded, with the build it was recorded with: an edited
    /// mod or a new Claude Code fails here until it is recorded again.
    #[test]
    fn the_embedded_mod_is_the_one_recorded() {
        assert_eq!(read("version").trim(), draft::CHECKED_WITH);
        assert_eq!(read("register.ts"), draft::SOURCE);
    }

    /// Each relay, read as the worker reads it, in the recorded order: the words as the box
    /// showed them, a long paste as its placeholder, the pasted picture from Claude Code's own
    /// store (and again when a recalled draft names it), and an empty box as none. A caret move
    /// and keys typed at a dialog change no words, so they relay nothing. Steady typing relays
    /// its words on the way too, as many times as the recorder's pace allowed, so the acts are
    /// held in order, not one relay each.
    #[test]
    fn each_relay_is_the_box_as_the_tui_showed_it() {
        let store: Value = serde_json::from_str(&read("store.json")).expect("the store");
        let cwd = store["cwd"].as_str().expect("its folder");
        let uid = u32::try_from(store["uid"].as_u64().expect("its uid")).expect("a uid");
        let stored = store["images"][0].as_str().expect("the stored picture");
        let root = tempfile::tempdir().expect("a temporary root");
        let picture = sessions::resolved(root.path()).join(stored);
        std::fs::create_dir_all(picture.parent().expect("its store")).expect("the store");
        std::fs::write(&picture, png()).expect("the picture");
        let tmpdir = root.path().to_str().expect("utf-8");
        let mut pictures = Pictures::new(uid, Path::new(cwd), "session-1");
        let mut seen = Vec::new();
        for line in read("drafts.jsonl").lines().filter(|l| !l.trim().is_empty()) {
            let record: Value = serde_json::from_str(line).expect("a record");
            let payload = record["payload"].to_string();
            let hook = Hook::parse(&payload).expect("a hook payload");
            assert_eq!(hook.event, DRAFT_EVENT);
            assert_eq!(hook.session_id.as_deref(), Some("session-1"));
            let Signal::Draft { text, tmpdir: recorded } = hook.signal() else {
                panic!("not a draft: {payload}")
            };
            assert!(recorded.is_some(), "the recorder moved the root: {payload}");
            // The recorded root is gone: the picture is under this test's.
            let (box_, missing) = pictures.draft(&text, Some(tmpdir), WallMs::ZERO);
            assert!(!missing, "{text:?}");
            let shown = box_.map(|d| {
                let images: Vec<(u32, String)> =
                    d.images.iter().map(|i| (i.n, i.image.media_type.clone())).collect();
                (d.text, images)
            });
            seen.push((record["act"].as_str().expect("an act").to_owned(), shown));
        }
        assert_eq!(pictures.dir(), picture.parent());
        let png = || vec![(2, "image/png".to_owned())];
        let pasted = "hell one\ntwo[Pasted text #1 +5 lines]";
        let want: Vec<(&str, Option<Shown>)> = vec![
            ("typed", Some(("hello".to_owned(), vec![]))),
            ("backspace", Some(("hell".to_owned(), vec![]))),
            ("short paste", Some(("hell one\ntwo".to_owned(), vec![]))),
            ("long paste", Some((pasted.to_owned(), vec![]))),
            ("image", Some((format!("{pasted}[Image #2]"), png()))),
            ("clear", None),
            ("recall", Some((format!("{pasted}[Image #2]"), png()))),
            ("back", None),
            ("submit", Some(("scenario".to_owned(), vec![]))),
            ("submit", None),
            ("pull back", Some(("queued".to_owned(), vec![]))),
            ("queue again", None),
            ("ask", Some(("scenario".to_owned(), vec![]))),
            ("ask", None),
        ];
        let mut rest = seen.iter();
        for (act, shown) in &want {
            assert!(
                rest.any(|(a, s)| a == act && s == shown),
                "{act}: {shown:?} is not relayed after the acts before it: {seen:#?}",
            );
        }
        for quiet in ["caret", "dialog keys"] {
            assert!(seen.iter().all(|(act, _)| act != quiet), "{quiet} relayed: {seen:#?}");
        }
    }

    /// The prompts queued behind a turn, as the transcript of the same run says them: the one
    /// queued while the turn ran, then none once the person pulled it back into the box.
    #[test]
    fn the_queue_is_the_transcript_s() {
        let dir = tempfile::tempdir().expect("a folder");
        let file = dir.path().join("session-1.jsonl");
        let mut out = std::fs::File::create(&file).expect("a transcript");
        let (mut conversation, mut tail) = (Conversation::default(), Tail::default());
        let mut told = Vec::new();
        for line in read("queue.jsonl").lines() {
            writeln!(out, "{line}").expect("written");
            for change in conversation.read(&mut tail, &file).expect("read") {
                if let Change::Queued(queued) = change {
                    told.push(queued.into_iter().map(|q| q.text).collect::<Vec<_>>());
                }
            }
        }
        assert_eq!(told, [vec!["queued".to_owned()], vec![]]);
    }

    /// The smallest PNG: one pixel.
    fn png() -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(b"\x08\x06\0\0\0\x1f\x15\xc4\x89");
        bytes
    }
}
