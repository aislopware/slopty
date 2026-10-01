use std::path::PathBuf;

use slopty_proto::ctl::Selection::{Clipboard, Primary};

use super::{Ends, Plan, Source, TEXT, best, pick, plan, sniff};

const PIPED: Ends = Ends { stdin_tty: false, stdout_tty: false };
const AT_A_TERMINAL: Ends = Ends { stdin_tty: true, stdout_tty: true };

fn of(name: &str, args: &str, ends: Ends) -> Plan {
    let args: Vec<String> = args.split_whitespace().map(str::to_owned).collect();
    plan(name, &args, ends).unwrap_or_else(|e| panic!("{name} {args:?}: {e:#}"))
}

fn refused(name: &str, args: &str) -> String {
    let args: Vec<String> = args.split_whitespace().map(str::to_owned).collect();
    match plan(name, &args, PIPED) {
        Ok(plan) => panic!("{name} {args:?} should fail, planned {plan:?}"),
        Err(e) => e.to_string(),
    }
}

fn paste(selection: super::Selection, kind: Option<&str>) -> Plan {
    Plan::Paste { selection, kind: kind.map(str::to_owned), newline: false, trim: false }
}

fn copy(selection: super::Selection, kind: Option<&str>, from: Source) -> Plan {
    Plan::Copy {
        selection,
        kind: kind.map(str::to_owned),
        from,
        trim: false,
        echo: false,
        append: false,
    }
}

/// What Claude Code runs on Linux: the picture check and read, its text read, and its copies.
#[test]
fn claude_codes_clipboard_commands_plan_as_it_means_them() {
    let any = PIPED;
    assert_eq!(
        of("xclip", "-selection clipboard -t TARGETS -o", any),
        Plan::List { selection: Clipboard, targets: true }
    );
    assert_eq!(
        of("xclip", "-selection clipboard -t image/png -o", any),
        paste(Clipboard, Some("image/png"))
    );
    assert_eq!(
        of("xclip", "-selection clipboard -t text/plain -o", any),
        paste(Clipboard, Some("text/plain"))
    );
    assert_eq!(of("wl-paste", "-l", any), Plan::List { selection: Clipboard, targets: false });
    assert_eq!(
        of("wl-paste", "--type image/png", any),
        Plan::Paste {
            selection: Clipboard,
            kind: Some("image/png".to_owned()),
            newline: true,
            trim: false
        }
    );
    assert_eq!(
        of("xclip", "-selection clipboard", any),
        copy(Clipboard, Some(TEXT), Source::Stdin)
    );
    assert_eq!(
        of("xsel", "--clipboard --input", AT_A_TERMINAL),
        copy(Clipboard, Some(TEXT), Source::Stdin)
    );
    assert_eq!(of("wl-copy", "", any), copy(Clipboard, None, Source::Stdin));
}

/// xclip takes any prefix of an option or a selection name, starts on the primary selection,
/// reads files named, and keeps the flags that shape what is copied.
#[test]
fn xclip_reads_options_as_the_x_toolkit_does() {
    assert_eq!(of("xclip", "-o", PIPED), paste(Primary, Some(TEXT)));
    assert_eq!(of("xclip", "-sel c -o", PIPED), paste(Clipboard, Some(TEXT)));
    assert_eq!(of("xclip", "-se clip -out", PIPED), paste(Clipboard, Some(TEXT)));
    assert_eq!(
        of("xclip", "-i notes.txt more.txt", PIPED),
        copy(
            Primary,
            Some(TEXT),
            Source::Files(vec![PathBuf::from("notes.txt"), PathBuf::from("more.txt")])
        )
    );
    assert_eq!(
        of("xclip", "-sel clipboard -rmlastnl -f -d :0 -l 1", PIPED),
        Plan::Copy {
            selection: Clipboard,
            kind: Some(TEXT.to_owned()),
            from: Source::Stdin,
            trim: true,
            echo: true,
            append: false
        }
    );
    assert!(refused("xclip", "-bogus").contains("unknown option"));
    assert!(refused("xclip", "-selection").contains("takes a value"));
    assert!(refused("xclip", "-selection nowhere").contains("no selection"));
}

/// xsel goes by its flags, grouped or long, and by which ends are piped when it has none: as a
/// pipe's middle it prints the old contents, then takes the new.
#[test]
fn xsel_goes_by_its_flags_then_by_its_ends() {
    let text = Some(TEXT);
    assert_eq!(of("xsel", "-bo", AT_A_TERMINAL), paste(Clipboard, text));
    assert_eq!(of("xsel", "--clipboard --output", PIPED), paste(Clipboard, text));
    assert_eq!(of("xsel", "-bi", AT_A_TERMINAL), copy(Clipboard, text, Source::Stdin));
    assert_eq!(of("xsel", "-bc", PIPED), Plan::Clear { selection: Clipboard });
    assert_eq!(
        of("xsel", "-a", AT_A_TERMINAL),
        Plan::Copy {
            selection: Primary,
            kind: text.map(str::to_owned),
            from: Source::Stdin,
            trim: false,
            echo: false,
            append: true
        }
    );
    let typed_in = Ends { stdin_tty: true, stdout_tty: false };
    assert_eq!(of("xsel", "", typed_in), paste(Primary, text));
    let piped_in = Ends { stdin_tty: false, stdout_tty: true };
    assert_eq!(of("xsel", "-b", piped_in), copy(Clipboard, text, Source::Stdin));
    assert_eq!(of("xsel", "", PIPED), Plan::Swap { selection: Primary });
    assert!(refused("xsel", "--follow").contains("not supported"));
    assert!(refused("xsel", "-q").contains("unknown option"));
}

/// wl-copy copies its words or standard input, with its type given in any of GNU's spellings;
/// wl-paste ends text with a newline unless told not to.
#[test]
fn wl_copy_and_wl_paste_take_gnu_flags() {
    let words = Source::Words(vec!["hello".to_owned(), "world".to_owned()]);
    assert_eq!(of("wl-copy", "hello world", PIPED), copy(Clipboard, None, words));
    for spelled in ["-t image/png", "--type image/png", "--type=image/png", "-timage/png"] {
        assert_eq!(
            of("wl-copy", &format!("{spelled} -p"), PIPED),
            copy(Primary, Some("image/png"), Source::Stdin),
            "{spelled}"
        );
    }
    let dash = Source::Words(vec!["-n".to_owned()]);
    assert_eq!(of("wl-copy", "-- -n", PIPED), copy(Clipboard, None, dash));
    assert_eq!(of("wl-copy", "--clear", PIPED), Plan::Clear { selection: Clipboard });
    assert_eq!(
        of("wl-paste", "-n -p", PIPED),
        Plan::Paste { selection: Primary, kind: None, newline: false, trim: false }
    );
    assert!(refused("wl-paste", "--watch cat").contains("not supported"));
    assert!(refused("wl-paste", "--type").contains("takes a value"));
    assert!(refused("wl-copy", "--typo").contains("unknown option"));
}

/// A read names a held type, or the first that begins with it; with none named it is text,
/// else the first held. A copy with no type is told from its bytes.
#[test]
fn types_are_picked_and_told_from_the_bytes() {
    let held = |types: &[&str]| types.iter().map(|&t| t.to_owned()).collect::<Vec<_>>();
    let both = held(&["image/png", TEXT, "UTF8_STRING"]);
    assert_eq!(pick(&both, "image").as_deref(), Some("image/png"));
    assert_eq!(pick(&both, "text").as_deref(), Some(TEXT));
    assert_eq!(pick(&both, "UTF8_STRING").as_deref(), Some("UTF8_STRING"));
    assert_eq!(pick(&both, "image/jpeg"), None);
    assert_eq!(best(&both).as_deref(), Some(TEXT));
    assert_eq!(best(&held(&["image/png"])).as_deref(), Some("image/png"));
    assert_eq!(best(&[]), None);

    assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), "image/png");
    assert_eq!(sniff(b"\xff\xd8\xff\xe0"), "image/jpeg");
    assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
    assert_eq!(sniff("héllo".as_bytes()), TEXT);
    assert_eq!(sniff(b"\xff\xfe\x00"), "application/octet-stream");
}
