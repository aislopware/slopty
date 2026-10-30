//! A line an orchestrating agent asks to run in a terminal it opened (`ui_terminal`),
//! typed into that terminal, never one that runs an agent or a project action. A plain one-line
//! command runs at once. A line of several lines, or one that asks for root or destroys (`sudo`,
//! `rm`, a forced push…), is left at the prompt for the person to read and run with Enter.

use gpui::App;

/// Programs a line is read before it runs with: they act as root or undo work.
const READ_FIRST: &[&str] = &[
    "sudo",
    "su",
    "doas",
    "rm",
    "rmdir",
    "dd",
    "shred",
    "truncate",
    "mkfs",
    "fdisk",
    "diskutil",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "kill",
    "killall",
    "pkill",
    "launchctl",
    "chmod",
    "chown",
    "mv",
];

/// Whether `line` is to be read before it runs: more than one line, or a command that acts as
/// root or destroys, anywhere in a pipeline or a list.
pub(super) fn read_first(line: &str) -> bool {
    let line = line.trim();
    if line.contains('\n') || writes_a_device(line) {
        return true;
    }
    line.split(['|', ';', '&', '(', ')', '`'])
        .map(|command| command.split_whitespace().collect::<Vec<_>>())
        .any(|words| {
            // `env X=1 sudo …` and `VAR=1 rm …` act as their program does.
            let words: Vec<&str> =
                words.into_iter().skip_while(|word| *word == "env" || word.contains('=')).collect();
            let Some(program) = words.first() else { return false };
            let program = program.rsplit('/').next().unwrap_or(program);
            READ_FIRST.contains(&program)
                || program.starts_with("mkfs.")
                || (program == "git" && destroys_in_git(&words[1..]))
        })
}

/// Whether `line` sends output onto a device (`> /dev/disk2`), past the harmless ones.
fn writes_a_device(line: &str) -> bool {
    const HARMLESS: [&str; 4] = ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"];
    line.match_indices('>').any(|(at, _)| {
        let target = line[at + 1..].trim_start_matches(['>', '&', '|']).trim_start();
        target.starts_with("/dev/") && !HARMLESS.iter().any(|ok| target.starts_with(ok))
    })
}

/// Whether git `args` throw work away: a forced push, a hard reset, a clean, a forced branch
/// delete, a checkout or restore over the tree.
fn destroys_in_git(args: &[&str]) -> bool {
    let has = |flag: &str| args.contains(&flag);
    match args.first().copied() {
        Some("push") => {
            has("--force") || has("-f") || args.iter().any(|a| a.starts_with("--force"))
        }
        Some("reset") => has("--hard"),
        Some("clean") => true,
        Some("branch") => has("-D"),
        Some("checkout" | "restore") => has("--") || has(".") || has("--force") || has("-f"),
        Some("stash") => has("drop") || has("clear"),
        _ => false,
    }
}

/// Type `line` into terminal `id`, and run it unless it is to be read first: whether the
/// terminal took it.
pub(super) fn send_line(id: &str, line: &str, cx: &mut App) -> bool {
    let line = line.trim_end_matches('\n');
    crate::live::run_line(id, line, !read_first(line), cx)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{read_first, send_line};

    /// What the app's terminal was handed: its id, the line, and whether Enter followed.
    type Typed = Rc<RefCell<Vec<(String, String, bool)>>>;

    fn serve(cx: &mut gpui::TestAppContext) -> Typed {
        let typed: Typed = Rc::default();
        let kept = Rc::clone(&typed);
        cx.update(|cx| {
            crate::live::serve_runs(cx, move |id, line, enter, _| {
                kept.borrow_mut().push((id.to_owned(), line.to_owned(), enter));
                true
            });
        });
        typed
    }

    /// A plain one-line command runs at once: typed, then Enter.
    #[gpui::test]
    fn a_plain_line_runs_at_once(cx: &mut gpui::TestAppContext) {
        let typed = serve(cx);
        for line in [
            "cargo test -p slopty-shell",
            "ls -la | grep rs",
            "git push origin main",
            "make 2> /dev/null",
        ] {
            assert!(!read_first(line), "{line}");
            assert!(cx.update(|cx| send_line("t1", &format!("{line}\n"), cx)));
        }
        let typed = typed.borrow();
        assert!(typed.iter().all(|(id, _, enter)| id == "t1" && *enter), "{typed:?}");
        assert_eq!(
            typed[0].1, "cargo test -p slopty-shell",
            "the fence's own newline is not typed"
        );
    }

    /// Several lines, or a line that acts as root or destroys, waits at the prompt for Enter.
    #[gpui::test]
    fn a_risky_or_long_line_waits_at_the_prompt(cx: &mut gpui::TestAppContext) {
        let typed = serve(cx);
        let risky = [
            "cd src\ncargo build",
            "sudo make install",
            "rm -rf target",
            "make && /bin/rm -rf out",
            "FORCE=1 env rm x",
            "git push --force-with-lease",
            "git reset --hard HEAD~1",
            "git clean -fdx",
            "echo hi > /dev/disk2",
            "mkfs.ext4 /dev/sdb1",
        ];
        for line in risky {
            assert!(read_first(line), "{line}");
            assert!(cx.update(|cx| send_line("t2", line, cx)));
        }
        assert!(typed.borrow().iter().all(|(_, _, enter)| !enter));
    }
}
