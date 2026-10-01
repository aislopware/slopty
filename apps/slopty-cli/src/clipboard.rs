//! `xclip`, `xsel`, `wl-copy` and `wl-paste`: a Linux session's clipboard commands, answered by
//! the worker's clipboard.
//!
//! A Linux worker has no desktop clipboard to sync, so it holds one itself and keeps it in step
//! with the client in front (`slopty_input::pasteboard::Held`). Its sessions find these four
//! names first on their `PATH` (`slopty_pty::shell_integration::CLIPBOARD_COMMANDS`), each the
//! `slopty` CLI read from `argv[0]`. They take the flags programs pass them: Claude Code's
//! `xclip -selection clipboard -t TARGETS -o` and `-t image/png -o` for a pasted picture, its
//! `xsel --clipboard --input` and `wl-copy` for a copy, an editor's `xclip -i`, a script's
//! `wl-paste -n`. The primary selection stays on the worker, since a Mac has none.
//!
//! When no worker answers (a shell that outlived it), the system's own command runs in its
//! place when there is one, as it would have without Slopty, and stderr says so otherwise.

use std::ffi::OsString;
use std::io::{IsTerminal as _, Read as _, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context as _, Result, bail};
use slopty_proto::ctl::{ClipAsk, CtlReply, CtlRequest, Selection};
use slopty_pty::shell_integration::BIN;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

use crate::workerctl;

/// The type of text, as `xclip` and `wl-copy` default to.
const TEXT: &str = "text/plain;charset=utf-8";

/// What a command line asks of the clipboard.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Plan {
    /// Print the types it holds, one a line.
    List {
        /// Which clipboard.
        selection: Selection,
        /// `xclip`'s `TARGETS` lists itself first.
        targets: bool,
    },
    /// Print one type's bytes.
    Paste {
        /// Which clipboard.
        selection: Selection,
        /// The type; `None` picks text, else the first type it holds (`wl-paste`).
        kind: Option<String>,
        /// Add a newline after text (`wl-paste` without `-n`).
        newline: bool,
        /// Drop a newline that ends it (`xclip -rmlastnl`, `xsel --trim`).
        trim: bool,
    },
    /// Copy what is read, from standard input or the files named, or the words given.
    Copy {
        /// Which clipboard.
        selection: Selection,
        /// The type; `None` tells it from the bytes (`wl-copy`).
        kind: Option<String>,
        /// Where the bytes come from.
        from: Source,
        /// Drop a newline that ends it.
        trim: bool,
        /// Also print what was copied (`xclip -filter`).
        echo: bool,
        /// Add to what it holds rather than replace it (`xsel --append`).
        append: bool,
    },
    /// Print what it holds, then copy standard input in its place (`xsel` with both ends piped).
    Swap {
        /// Which clipboard.
        selection: Selection,
    },
    /// Empty it.
    Clear {
        /// Which clipboard.
        selection: Selection,
    },
    /// Print this, and do nothing.
    Say(String),
}

/// Where a copy's bytes come from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Source {
    /// Standard input.
    Stdin,
    /// These files, one after another.
    Files(Vec<PathBuf>),
    /// These words, a space between each (`wl-copy hello world`).
    Words(Vec<String>),
}

/// Whether standard input and output are terminals, which `xsel` decides its direction by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ends {
    /// Standard input is a terminal.
    pub stdin_tty: bool,
    /// Standard output is a terminal.
    pub stdout_tty: bool,
}

/// Run as `name`, one of `CLIPBOARD_COMMANDS`, with `args`.
pub async fn run(name: &str, data_dir: &Path, args: Vec<OsString>) -> Result<ExitCode> {
    let ends = Ends {
        stdin_tty: std::io::stdin().is_terminal(),
        stdout_tty: std::io::stdout().is_terminal(),
    };
    let words: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let plan = match plan(name, &words, ends) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("{name}: {e:#}");
            return Ok(ExitCode::from(2));
        }
    };
    let socket = workerctl::socket(data_dir);
    match carry_out(&socket, plan).await {
        Ok(code) => Ok(code),
        Err(Failed::Said(message)) => {
            eprintln!("{name}: {message}");
            Ok(ExitCode::FAILURE)
        }
        Err(Failed::NoWorker(e)) => {
            tracing::debug!(error = %e, "no worker to hold the clipboard");
            Err(system_command(name, &args))
        }
        Err(Failed::Other(e)) => Err(e),
    }
}

/// How a request went wrong.
enum Failed {
    /// The worker answered with a refusal, which is the command's error.
    Said(String),
    /// No worker answered.
    NoWorker(anyhow::Error),
    /// Anything else.
    Other(anyhow::Error),
}

impl From<anyhow::Error> for Failed {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

impl From<std::io::Error> for Failed {
    fn from(e: std::io::Error) -> Self {
        Self::Other(e.into())
    }
}

async fn carry_out(socket: &Path, plan: Plan) -> Result<ExitCode, Failed> {
    let mut out = std::io::stdout();
    match plan {
        Plan::Say(text) => out.write_all(text.as_bytes())?,
        Plan::List { selection, targets } => {
            let types = types(socket, selection).await?;
            let shown = targets.then_some("TARGETS".to_owned()).into_iter().chain(types);
            for kind in shown {
                writeln!(out, "{kind}")?;
            }
        }
        Plan::Paste { selection, kind, newline, trim } => {
            let kind = match kind {
                Some(kind) => pick(&types(socket, selection).await?, &kind).unwrap_or(kind),
                None => best(&types(socket, selection).await?)
                    .ok_or_else(|| Failed::Said("nothing is copied".to_owned()))?,
            };
            let mut bytes = read(socket, selection, &kind).await?;
            if trim && bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            out.write_all(&bytes)?;
            if newline && is_text(&kind) && !bytes.ends_with(b"\n") {
                out.write_all(b"\n")?;
            }
        }
        Plan::Copy { selection, kind, from, trim, echo, append } => {
            let mut bytes = gather(&from)?;
            if trim && bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            if echo {
                out.write_all(&bytes)?;
            }
            if append {
                let held = read(socket, selection, TEXT).await;
                if let Ok(mut held) = held {
                    held.append(&mut bytes);
                    bytes = held;
                }
            }
            let kind = kind.unwrap_or_else(|| sniff(&bytes).to_owned());
            write(socket, selection, &kind, &bytes).await?;
        }
        Plan::Swap { selection } => {
            if let Ok(held) = read(socket, selection, TEXT).await {
                out.write_all(&held)?;
            }
            let bytes = gather(&Source::Stdin)?;
            write(socket, selection, TEXT, &bytes).await?;
        }
        Plan::Clear { selection } => {
            expect_ok(exchange(socket, &ClipAsk::Clear { selection }, &[]).await?)?;
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// What `name` with `args` asks for.
pub fn plan(name: &str, args: &[String], ends: Ends) -> Result<Plan> {
    match name {
        "xclip" => xclip(args),
        "xsel" => xsel(args, ends),
        "wl-copy" => wl_copy(args),
        "wl-paste" => wl_paste(args),
        _ => bail!("not a clipboard command"),
    }
}

/// `xclip`: X toolkit options, each a word of its own and any unambiguous prefix of its name.
fn xclip(args: &[String]) -> Result<Plan> {
    const NAMES: [&str; 14] = [
        "in",
        "out",
        "filter",
        "loops",
        "display",
        "selection",
        "silent",
        "quiet",
        "verbose",
        "version",
        "target",
        "rmlastnl",
        "noutf8",
        "help",
    ];
    let (mut output, mut selection, mut kind, mut trim, mut echo) =
        (false, Selection::Primary, None, false, false);
    let mut files = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let Some(flag) = arg.strip_prefix('-').filter(|f| !f.is_empty()) else {
            files.push(PathBuf::from(arg));
            continue;
        };
        let flag = flag.strip_prefix('-').unwrap_or(flag);
        let Some(&name) = NAMES.iter().find(|n| n.starts_with(flag)) else {
            bail!("unknown option -{flag}");
        };
        let mut value = || rest.next().with_context(|| format!("-{name} takes a value"));
        match name {
            "in" => output = false,
            "out" => output = true,
            "filter" => echo = true,
            "selection" => selection = x_selection(value()?)?,
            "target" => kind = Some(value()?.clone()),
            "rmlastnl" => trim = true,
            "loops" | "display" => {
                value()?;
            }
            "version" => return Ok(Plan::Say("xclip version 0.13 (Slopty)\n".to_owned())),
            "help" => return Ok(Plan::Say(XCLIP_HELP.to_owned())),
            _ => {}
        }
    }
    if output {
        return Ok(match kind.as_deref() {
            Some("TARGETS") => Plan::List { selection, targets: true },
            _ => Plan::Paste {
                selection,
                kind: Some(kind.unwrap_or_else(|| TEXT.to_owned())),
                newline: false,
                trim,
            },
        });
    }
    let from = if files.is_empty() { Source::Stdin } else { Source::Files(files) };
    let kind = Some(kind.unwrap_or_else(|| TEXT.to_owned()));
    Ok(Plan::Copy { selection, kind, from, trim, echo, append: false })
}

/// `xclip`'s selection names, each any prefix of itself: `c`, `clip`, `clipboard`.
fn x_selection(name: &str) -> Result<Selection> {
    match name.chars().next() {
        Some('c') => Ok(Selection::Clipboard),
        Some('p' | 's') => Ok(Selection::Primary),
        _ => bail!("no selection called {name}"),
    }
}

const XCLIP_HELP: &str = "Usage: xclip [OPTION] [FILE]...\n\
    Access the clipboard Slopty's worker holds, from standard input or files.\n\
    \x20 -i, -in          read text into the selection (default)\n\
    \x20 -o, -out         print the selection\n\
    \x20 -f, -filter      also print what is read in\n\
    \x20 -selection       primary (default) or clipboard\n\
    \x20 -t, -target      the type, or TARGETS to list them\n\
    \x20 -rmlastnl        drop a trailing newline\n";

/// `xsel`: GNU-style flags, short ones groupable.
fn xsel(args: &[String], ends: Ends) -> Result<Plan> {
    let (mut input, mut output, mut clear, mut append, mut trim) =
        (false, false, false, false, false);
    let mut selection = Selection::Primary;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let flags: Vec<String> = if let Some(long) = arg.strip_prefix("--") {
            vec![long.to_owned()]
        } else if let Some(short) = arg.strip_prefix('-').filter(|s| !s.is_empty()) {
            short.chars().map(String::from).collect()
        } else {
            bail!("unexpected argument {arg}");
        };
        for flag in flags {
            match flag.as_str() {
                "i" | "input" => input = true,
                "o" | "output" => output = true,
                "a" | "append" => {
                    input = true;
                    append = true;
                }
                "c" | "clear" | "d" | "delete" => clear = true,
                "p" | "primary" | "s" | "secondary" => selection = Selection::Primary,
                "b" | "clipboard" => selection = Selection::Clipboard,
                "trim" => trim = true,
                "n" | "nodetach" | "k" | "keep" | "v" | "verbose" | "z" | "zeroflush" => {}
                "display" | "t" | "selectionTimeout" | "l" | "logfile" => {
                    rest.next().with_context(|| format!("--{flag} takes a value"))?;
                }
                "f" | "follow" | "x" | "exchange" => bail!("--{flag} is not supported here"),
                "version" => return Ok(Plan::Say("xsel version 1.2.1 (Slopty)\n".to_owned())),
                "h" | "help" => return Ok(Plan::Say(XSEL_HELP.to_owned())),
                other => bail!("unknown option {other}"),
            }
        }
    }
    if clear {
        return Ok(Plan::Clear { selection });
    }
    // As xsel does: with neither asked, read a piped input and print to a piped output.
    if !input && !output {
        input = !ends.stdin_tty;
        output = !ends.stdout_tty;
    }
    Ok(match (input, output) {
        (true, true) if !append => Plan::Swap { selection },
        (true, _) => Plan::Copy {
            selection,
            kind: Some(TEXT.to_owned()),
            from: Source::Stdin,
            trim: false,
            echo: false,
            append,
        },
        (false, _) => Plan::Paste { selection, kind: Some(TEXT.to_owned()), newline: false, trim },
    })
}

const XSEL_HELP: &str = "Usage: xsel [options]\n\
    Access the clipboard Slopty's worker holds.\n\
    \x20 -i, --input      read standard input into the selection\n\
    \x20 -o, --output     print the selection\n\
    \x20 -a, --append     add standard input to the selection\n\
    \x20 -c, --clear      empty the selection\n\
    \x20 -p, --primary    the primary selection (default)\n\
    \x20 -b, --clipboard  the clipboard\n";

/// A GNU-style flag's value: `--type=x`, `--type x`, `-t x` or `-tx`.
fn value_of<'a>(
    arg: &'a str,
    short: char,
    long: &str,
    rest: &mut impl Iterator<Item = &'a String>,
) -> Option<Result<String>> {
    let missing = || anyhow::anyhow!("--{long} takes a value");
    if let Some(tail) = arg.strip_prefix("--").and_then(|a| a.strip_prefix(long)) {
        return match tail.strip_prefix('=') {
            Some(value) => Some(Ok(value.to_owned())),
            None if tail.is_empty() => Some(rest.next().cloned().ok_or_else(missing)),
            None => None,
        };
    }
    let tail = arg.strip_prefix('-').and_then(|a| a.strip_prefix(short))?;
    Some(if tail.is_empty() {
        rest.next().cloned().ok_or_else(missing)
    } else {
        Ok(tail.to_owned())
    })
}

/// `wl-copy [options] [text...]`.
fn wl_copy(args: &[String]) -> Result<Plan> {
    let (mut selection, mut kind, mut trim, mut clear) = (Selection::Clipboard, None, false, false);
    let mut words = Vec::new();
    let mut rest = args.iter();
    let mut flags_done = false;
    while let Some(arg) = rest.next() {
        if flags_done || !arg.starts_with('-') || arg == "-" {
            words.push(arg.clone());
            continue;
        }
        if let Some(value) = value_of(arg, 't', "type", &mut rest) {
            kind = Some(value?);
            continue;
        }
        if let Some(value) = value_of(arg, 's', "seat", &mut rest) {
            value?;
            continue;
        }
        match arg.as_str() {
            "--" => flags_done = true,
            "-p" | "--primary" => selection = Selection::Primary,
            "-n" | "--trim-newline" => trim = true,
            "-c" | "--clear" => clear = true,
            "-o" | "--paste-once" | "-f" | "--foreground" | "--regular" | "--sensitive" => {}
            "-v" | "--version" => return Ok(Plan::Say("wl-clipboard 2.2.1 (Slopty)\n".to_owned())),
            "-h" | "--help" => return Ok(Plan::Say(WL_COPY_HELP.to_owned())),
            other => bail!("unknown option {other}"),
        }
    }
    if clear {
        return Ok(Plan::Clear { selection });
    }
    let from = if words.is_empty() { Source::Stdin } else { Source::Words(words) };
    Ok(Plan::Copy { selection, kind, from, trim, echo: false, append: false })
}

const WL_COPY_HELP: &str = "Usage: wl-copy [options] [text...]\n\
    Copy to the clipboard Slopty's worker holds.\n\
    \x20 -p, --primary       the primary selection\n\
    \x20 -n, --trim-newline  drop a trailing newline\n\
    \x20 -t, --type TYPE     the MIME type (told from the bytes otherwise)\n\
    \x20 -c, --clear         empty the clipboard\n";

/// `wl-paste [options]`.
fn wl_paste(args: &[String]) -> Result<Plan> {
    let (mut selection, mut kind, mut newline, mut list) =
        (Selection::Clipboard, None, true, false);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if let Some(value) = value_of(arg, 't', "type", &mut rest) {
            kind = Some(value?);
            continue;
        }
        if let Some(value) = value_of(arg, 's', "seat", &mut rest) {
            value?;
            continue;
        }
        match arg.as_str() {
            "-l" | "--list-types" => list = true,
            "-p" | "--primary" => selection = Selection::Primary,
            "-n" | "--no-newline" => newline = false,
            "-w" | "--watch" => bail!("--watch is not supported here"),
            "-v" | "--version" => return Ok(Plan::Say("wl-clipboard 2.2.1 (Slopty)\n".to_owned())),
            "-h" | "--help" => return Ok(Plan::Say(WL_PASTE_HELP.to_owned())),
            other => bail!("unknown option {other}"),
        }
    }
    if list {
        return Ok(Plan::List { selection, targets: false });
    }
    Ok(Plan::Paste { selection, kind, newline, trim: false })
}

const WL_PASTE_HELP: &str = "Usage: wl-paste [options]\n\
    Paste from the clipboard Slopty's worker holds.\n\
    \x20 -l, --list-types  list the types\n\
    \x20 -p, --primary     the primary selection\n\
    \x20 -n, --no-newline  add no newline after text\n\
    \x20 -t, --type TYPE   the type, or a prefix of one (text, image)\n";

/// Whether `kind` is text, which `wl-paste` ends with a newline.
fn is_text(kind: &str) -> bool {
    kind.starts_with("text/") || ["UTF8_STRING", "STRING", "TEXT"].contains(&kind)
}

/// The type of the held `types` a read of `wanted` means: itself, else the first type it
/// begins (`wl-paste -t text`, `-t image`).
fn pick(types: &[String], wanted: &str) -> Option<String> {
    if types.iter().any(|t| t == wanted) {
        return Some(wanted.to_owned());
    }
    let prefix = format!("{wanted}/");
    types.iter().find(|t| t.starts_with(&prefix)).cloned()
}

/// What `wl-paste` with no type prints: text when it holds any, else its first type.
fn best(types: &[String]) -> Option<String> {
    types.iter().find(|t| t.as_str() == TEXT).or_else(|| types.first()).cloned()
}

/// The type `bytes` are, as `wl-copy` tells it when none is given.
fn sniff(bytes: &[u8]) -> &'static str {
    const MAGIC: [(&[u8], &str); 6] = [
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"GIF8", "image/gif"),
        (b"II*\0", "image/tiff"),
        (b"MM\0*", "image/tiff"),
        (b"%PDF-", "application/pdf"),
    ];
    if let Some((_, kind)) = MAGIC.iter().find(|(magic, _)| bytes.starts_with(magic)) {
        return kind;
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return "image/webp";
    }
    if std::str::from_utf8(bytes).is_ok() { TEXT } else { "application/octet-stream" }
}

/// A copy's bytes, read whole.
fn gather(from: &Source) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    match from {
        Source::Stdin => {
            std::io::stdin().lock().read_to_end(&mut bytes).context("read standard input")?;
        }
        Source::Files(files) => {
            for file in files {
                let mut read =
                    std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
                bytes.append(&mut read);
            }
        }
        Source::Words(words) => bytes = words.join(" ").into_bytes(),
    }
    Ok(bytes)
}

async fn types(socket: &Path, selection: Selection) -> Result<Vec<String>, Failed> {
    match exchange(socket, &ClipAsk::Types { selection }, &[]).await? {
        (CtlReply::ClipTypes { types }, _) => Ok(types),
        (other, _) => Err(unexpected(other)),
    }
}

async fn read(socket: &Path, selection: Selection, kind: &str) -> Result<Vec<u8>, Failed> {
    let ask = ClipAsk::Read { selection, kind: kind.to_owned() };
    match exchange(socket, &ask, &[]).await? {
        (CtlReply::ClipData { .. }, bytes) => Ok(bytes),
        (other, _) => Err(unexpected(other)),
    }
}

async fn write(
    socket: &Path,
    selection: Selection,
    kind: &str,
    bytes: &[u8],
) -> Result<(), Failed> {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let ask = ClipAsk::Write { selection, kind: kind.to_owned(), len };
    expect_ok(exchange(socket, &ask, bytes).await?)
}

fn expect_ok(answer: (CtlReply, Vec<u8>)) -> Result<(), Failed> {
    match answer.0 {
        CtlReply::Ok { .. } => Ok(()),
        other => Err(unexpected(other)),
    }
}

fn unexpected(reply: CtlReply) -> Failed {
    match reply {
        CtlReply::Error { message } => Failed::Said(message),
        other => Failed::Other(anyhow::anyhow!("the worker answered {other:?}")),
    }
}

/// One clipboard request: its line and `body`, then the reply line and the bytes it announces.
async fn exchange(
    socket: &Path,
    ask: &ClipAsk,
    body: &[u8],
) -> Result<(CtlReply, Vec<u8>), Failed> {
    let stream = tokio::net::UnixStream::connect(socket).await.map_err(|e| {
        Failed::NoWorker(anyhow::Error::new(e).context(socket.display().to_string()))
    })?;
    let (rd, mut wr) = stream.into_split();
    let mut line =
        serde_json::to_vec(&CtlRequest::Clip(ask.clone())).map_err(anyhow::Error::new)?;
    line.push(b'\n');
    wr.write_all(&line).await?;
    wr.write_all(body).await?;
    wr.shutdown().await?;
    let mut rd = BufReader::new(rd);
    let mut reply = String::new();
    rd.read_line(&mut reply).await?;
    let reply: CtlReply = serde_json::from_str(reply.trim())
        .with_context(|| format!("the worker's reply {reply:?}"))?;
    let mut bytes = Vec::new();
    if let CtlReply::ClipData { len } = reply {
        rd.take(len).read_to_end(&mut bytes).await?;
        if u64::try_from(bytes.len()).ok() != Some(len) {
            return Err(Failed::Other(anyhow::anyhow!(
                "the paste ended after {} of {len} bytes",
                bytes.len()
            )));
        }
    }
    Ok((reply, bytes))
}

/// The system's own `name`, run in this process's place: the first on `PATH` past Slopty's
/// directory that is not this binary.
fn system_command(name: &str, args: &[OsString]) -> anyhow::Error {
    let ours = std::env::var_os(BIN).map(PathBuf::from);
    let me = std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(p).ok());
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .filter(|dir| ours.as_ref() != Some(dir))
            .map(|dir| dir.join(name))
            .find(|c| c.is_file() && std::fs::canonicalize(c).ok().as_ref() != me.as_ref())
    });
    let Some(real) = found else {
        return anyhow::anyhow!(
            "no clipboard: the Slopty worker is not running, and no other {name} is here"
        );
    };
    let e = Command::new(&real).args(args).exec();
    anyhow::anyhow!("run {}: {e}", real.display())
}

#[cfg(test)]
mod tests;
