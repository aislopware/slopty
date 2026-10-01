//! Golden JSON lines of the control socket's clipboard requests (`slopty_proto::ctl::ClipAsk`),
//! which a Linux session's `xclip`, `xsel`, `wl-copy` and `wl-paste` send. A changed snapshot is
//! a change to what those commands and the worker say to each other: accept it deliberately
//! (`cargo insta review`).

#[cfg(test)]
mod golden_ctl_clip {
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use slopty_proto::ctl::{ClipAsk, CtlReply, CtlRequest, Selection};

    #[track_caller]
    fn snap<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(name: &str, msg: &T) {
        let line = serde_json::to_string(msg).expect("encodes");
        assert_eq!(serde_json::from_str::<T>(&line).ok().as_ref(), Some(msg), "{name} reads back");
        insta::assert_snapshot!(name, line);
    }

    #[test]
    fn requests() {
        let primary = Selection::Primary;
        let clip = |ask| CtlRequest::Clip(ask);
        snap("ctl_clip_types", &clip(ClipAsk::Types { selection: Selection::Clipboard }));
        snap(
            "ctl_clip_read",
            &clip(ClipAsk::Read { selection: Selection::Clipboard, kind: "image/png".to_owned() }),
        );
        snap(
            "ctl_clip_write",
            &clip(ClipAsk::Write { selection: primary, kind: "UTF8_STRING".to_owned(), len: 11 }),
        );
        snap("ctl_clip_clear", &clip(ClipAsk::Clear { selection: primary }));
    }

    #[test]
    fn replies() {
        let types = vec!["text/plain;charset=utf-8".to_owned(), "UTF8_STRING".to_owned()];
        snap("ctl_reply_clip_types", &CtlReply::ClipTypes { types });
        snap("ctl_reply_clip_data", &CtlReply::ClipData { len: 4096 });
    }
}
