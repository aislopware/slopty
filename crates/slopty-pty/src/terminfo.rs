//! ghostty's terminfo entry, and compiling it into the user's database.
//!
//! Ported from `vendor/ghostty/src/terminfo/ghostty.zig` and rendered the way its `Source.zig`
//! renders it. A shell only believes what its terminfo entry claims, so this is what makes
//! `TERM=xterm-ghostty` mean 24-bit colour, styled underlines, bracketed paste, the Kitty
//! keyboard protocol and the rest of what the engine actually implements.
//!
//! [`source`] is snapshot-tested, so bumping the vendored ghostty shows the protocol change as
//! a diff rather than as a silent difference in what programs are told.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Stdio;

/// What a capability carries (ghostty's `Source.Capability.Value`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Value {
    /// Explicitly absent: written with a `@` suffix.
    Canceled,
    /// Present, and therefore true.
    Boolean,
    /// An unsigned decimal integer.
    Numeric(u32),
    /// An escape sequence, possibly with `%` parameters.
    Str(&'static str),
}

/// The names this entry answers to. The first is what `TERM` is set to: the `xterm-` prefix is
/// there because programs sniff it to decide whether 256 colours are on offer.
pub const NAMES: [&str; 3] = ["xterm-ghostty", "ghostty", "Ghostty"];

/// Every capability, in ghostty's order — the order they are written in.
pub const CAPABILITIES: [(&str, Value); 270] = [
    ("am", Value::Boolean),
    ("bce", Value::Boolean),
    ("ccc", Value::Boolean),
    ("hs", Value::Boolean),
    ("km", Value::Boolean),
    ("mc5i", Value::Boolean),
    ("mir", Value::Boolean),
    ("msgr", Value::Boolean),
    ("npc", Value::Boolean),
    ("xenl", Value::Boolean),
    ("AX", Value::Boolean),
    ("Tc", Value::Boolean),
    ("Su", Value::Boolean),
    ("XT", Value::Boolean),
    ("fullkbd", Value::Boolean),
    ("colors", Value::Numeric(256)),
    ("cols", Value::Numeric(80)),
    ("it", Value::Numeric(8)),
    ("lines", Value::Numeric(24)),
    ("pairs", Value::Numeric(32767)),
    ("acsc", Value::Str(r"++\,\,--..00``aaffgghhiijjkkllmmnnooppqqrrssttuuvvwwxxyyzz{{||}}~~")),
    ("Smulx", Value::Str(r"\E[4:%p1%dm")),
    ("Smol", Value::Str(r"\E[53m")),
    ("Rmol", Value::Str(r"\E[55m")),
    ("Setulc", Value::Str(r"\E[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m")),
    ("Ss", Value::Str(r"\E[%p1%d q")),
    ("Se", Value::Str(r"\E[0 q")),
    ("Ms", Value::Str(r"\E]52;%p1%s;%p2%s\007")),
    ("Sync", Value::Str(r"\E[?2026%?%p1%{1}%-%tl%eh%;")),
    ("BD", Value::Str(r"\E[?2004l")),
    ("BE", Value::Str(r"\E[?2004h")),
    ("PS", Value::Str(r"\E[200~")),
    ("PE", Value::Str(r"\E[201~")),
    ("XM", Value::Str(r"\E[?1006;1000%?%p1%{1}%=%th%el%;")),
    ("xm", Value::Str(r"\E[<%i%p3%d;%p1%d;%p2%d;%?%p4%tM%em%;")),
    ("RV", Value::Str(r"\E[>c")),
    ("rv", Value::Str(r"\E\\[[0-9]+;[0-9]+;[0-9]+c")),
    ("XR", Value::Str(r"\E[>0q")),
    ("xr", Value::Str(r"\EP>\\|[ -~]+a\E\\")),
    ("Enmg", Value::Str(r"\E[?69h")),
    ("Dsmg", Value::Str(r"\E[?69l")),
    ("Clmg", Value::Str(r"\E[s")),
    ("Cmg", Value::Str(r"\E[%i%p1%d;%p2%ds")),
    ("clear", Value::Str(r"\E[H\E[2J")),
    ("E3", Value::Str(r"\E[3J")),
    ("fe", Value::Str(r"\E[?1004h")),
    ("fd", Value::Str(r"\E[?1004l")),
    ("kxIN", Value::Str(r"\E[I")),
    ("kxOUT", Value::Str(r"\E[O")),
    ("bel", Value::Str(r"^G")),
    ("blink", Value::Str(r"\E[5m")),
    ("bold", Value::Str(r"\E[1m")),
    ("cbt", Value::Str(r"\E[Z")),
    ("civis", Value::Str(r"\E[?25l")),
    ("cnorm", Value::Str(r"\E[?12l\E[?25h")),
    ("cr", Value::Str(r"\r")),
    ("csr", Value::Str(r"\E[%i%p1%d;%p2%dr")),
    ("cub", Value::Str(r"\E[%p1%dD")),
    ("cub1", Value::Str(r"^H")),
    ("cud", Value::Str(r"\E[%p1%dB")),
    ("cud1", Value::Str(r"^J")),
    ("cuf", Value::Str(r"\E[%p1%dC")),
    ("cuf1", Value::Str(r"\E[C")),
    ("cup", Value::Str(r"\E[%i%p1%d;%p2%dH")),
    ("cuu", Value::Str(r"\E[%p1%dA")),
    ("cuu1", Value::Str(r"\E[A")),
    ("cvvis", Value::Str(r"\E[?12;25h")),
    ("dch", Value::Str(r"\E[%p1%dP")),
    ("dch1", Value::Str(r"\E[P")),
    ("dim", Value::Str(r"\E[2m")),
    ("dl", Value::Str(r"\E[%p1%dM")),
    ("dl1", Value::Str(r"\E[M")),
    ("dsl", Value::Str(r"\E]2;\007")),
    ("ech", Value::Str(r"\E[%p1%dX")),
    ("ed", Value::Str(r"\E[J")),
    ("el", Value::Str(r"\E[K")),
    ("el1", Value::Str(r"\E[1K")),
    ("flash", Value::Str(r"\E[?5h$<100/>\E[?5l")),
    ("fsl", Value::Str(r"^G")),
    ("home", Value::Str(r"\E[H")),
    ("hpa", Value::Str(r"\E[%i%p1%dG")),
    ("ht", Value::Str(r"^I")),
    ("hts", Value::Str(r"\EH")),
    ("ich", Value::Str(r"\E[%p1%d@")),
    ("ich1", Value::Str(r"\E[@")),
    ("il", Value::Str(r"\E[%p1%dL")),
    ("il1", Value::Str(r"\E[L")),
    ("ind", Value::Str(r"\n")),
    ("indn", Value::Str(r"\E[%p1%dS")),
    (
        "initc",
        Value::Str(
            r"\E]4;%p1%d;rgb\:%p2%{255}%*%{1000}%/%2.2X/%p3%{255}%*%{1000}%/%2.2X/%p4%{255}%*%{1000}%/%2.2X\E\\",
        ),
    ),
    ("invis", Value::Str(r"\E[8m")),
    ("oc", Value::Str(r"\E]104\007")),
    ("op", Value::Str(r"\E[39;49m")),
    ("rc", Value::Str(r"\E8")),
    ("rep", Value::Str(r"%p1%c\E[%p2%{1}%-%db")),
    ("rev", Value::Str(r"\E[7m")),
    ("ri", Value::Str(r"\EM")),
    ("rin", Value::Str(r"\E[%p1%dT")),
    ("ritm", Value::Str(r"\E[23m")),
    ("rmacs", Value::Str(r"\E(B")),
    ("rmam", Value::Str(r"\E[?7l")),
    ("rmcup", Value::Str(r"\E[?1049l")),
    ("rmir", Value::Str(r"\E[4l")),
    ("rmkx", Value::Str(r"\E[?1l\E>")),
    ("rmso", Value::Str(r"\E[27m")),
    ("rmul", Value::Str(r"\E[24m")),
    ("rmxx", Value::Str(r"\E[29m")),
    ("setab", Value::Str(r"\E[%?%p1%{8}%<%t4%p1%d%e%p1%{16}%<%t10%p1%{8}%-%d%e48;5;%p1%d%;m")),
    ("setaf", Value::Str(r"\E[%?%p1%{8}%<%t3%p1%d%e%p1%{16}%<%t9%p1%{8}%-%d%e38;5;%p1%d%;m")),
    ("setrgbb", Value::Str(r"\E[48:2:%p1%d:%p2%d:%p3%dm")),
    ("setrgbf", Value::Str(r"\E[38:2:%p1%d:%p2%d:%p3%dm")),
    (
        "sgr",
        Value::Str(
            r"%?%p9%t\E(0%e\E(B%;\E[0%?%p6%t;1%;%?%p5%t;2%;%?%p2%t;4%;%?%p1%p3%|%t;7%;%?%p4%t;5%;%?%p7%t;8%;m",
        ),
    ),
    ("sgr0", Value::Str(r"\E(B\E[m")),
    ("sitm", Value::Str(r"\E[3m")),
    ("smacs", Value::Str(r"\E(0")),
    ("smam", Value::Str(r"\E[?7h")),
    ("smcup", Value::Str(r"\E[?1049h")),
    ("smir", Value::Str(r"\E[4h")),
    ("smkx", Value::Str(r"\E[?1h\E=")),
    ("smso", Value::Str(r"\E[7m")),
    ("smul", Value::Str(r"\E[4m")),
    ("smxx", Value::Str(r"\E[9m")),
    ("tbc", Value::Str(r"\E[3g")),
    ("tsl", Value::Str(r"\E]2;")),
    ("u6", Value::Str(r"\E[%i%d;%dR")),
    ("u7", Value::Str(r"\E[6n")),
    ("u8", Value::Str(r"\E[?%[;0123456789]c")),
    ("u9", Value::Str(r"\E[c")),
    ("vpa", Value::Str(r"\E[%i%p1%dd")),
    ("kDC", Value::Str(r"\E[3;2~")),
    ("kDC3", Value::Str(r"\E[3;3~")),
    ("kDC4", Value::Str(r"\E[3;4~")),
    ("kDC5", Value::Str(r"\E[3;5~")),
    ("kDC6", Value::Str(r"\E[3;6~")),
    ("kDC7", Value::Str(r"\E[3;7~")),
    ("kDN", Value::Str(r"\E[1;2B")),
    ("kDN3", Value::Str(r"\E[1;3B")),
    ("kDN4", Value::Str(r"\E[1;4B")),
    ("kDN5", Value::Str(r"\E[1;5B")),
    ("kDN6", Value::Str(r"\E[1;6B")),
    ("kDN7", Value::Str(r"\E[1;7B")),
    ("kEND", Value::Str(r"\E[1;2F")),
    ("kEND3", Value::Str(r"\E[1;3F")),
    ("kEND4", Value::Str(r"\E[1;4F")),
    ("kEND5", Value::Str(r"\E[1;5F")),
    ("kEND6", Value::Str(r"\E[1;6F")),
    ("kEND7", Value::Str(r"\E[1;7F")),
    ("kHOM", Value::Str(r"\E[1;2H")),
    ("kHOM3", Value::Str(r"\E[1;3H")),
    ("kHOM4", Value::Str(r"\E[1;4H")),
    ("kHOM5", Value::Str(r"\E[1;5H")),
    ("kHOM6", Value::Str(r"\E[1;6H")),
    ("kHOM7", Value::Str(r"\E[1;7H")),
    ("kIC", Value::Str(r"\E[2;2~")),
    ("kIC3", Value::Str(r"\E[2;3~")),
    ("kIC4", Value::Str(r"\E[2;4~")),
    ("kIC5", Value::Str(r"\E[2;5~")),
    ("kIC6", Value::Str(r"\E[2;6~")),
    ("kIC7", Value::Str(r"\E[2;7~")),
    ("kLFT", Value::Str(r"\E[1;2D")),
    ("kLFT3", Value::Str(r"\E[1;3D")),
    ("kLFT4", Value::Str(r"\E[1;4D")),
    ("kLFT5", Value::Str(r"\E[1;5D")),
    ("kLFT6", Value::Str(r"\E[1;6D")),
    ("kLFT7", Value::Str(r"\E[1;7D")),
    ("kNXT", Value::Str(r"\E[6;2~")),
    ("kNXT3", Value::Str(r"\E[6;3~")),
    ("kNXT4", Value::Str(r"\E[6;4~")),
    ("kNXT5", Value::Str(r"\E[6;5~")),
    ("kNXT6", Value::Str(r"\E[6;6~")),
    ("kNXT7", Value::Str(r"\E[6;7~")),
    ("kPRV", Value::Str(r"\E[5;2~")),
    ("kPRV3", Value::Str(r"\E[5;3~")),
    ("kPRV4", Value::Str(r"\E[5;4~")),
    ("kPRV5", Value::Str(r"\E[5;5~")),
    ("kPRV6", Value::Str(r"\E[5;6~")),
    ("kPRV7", Value::Str(r"\E[5;7~")),
    ("kRIT", Value::Str(r"\E[1;2C")),
    ("kRIT3", Value::Str(r"\E[1;3C")),
    ("kRIT4", Value::Str(r"\E[1;4C")),
    ("kRIT5", Value::Str(r"\E[1;5C")),
    ("kRIT6", Value::Str(r"\E[1;6C")),
    ("kRIT7", Value::Str(r"\E[1;7C")),
    ("kUP", Value::Str(r"\E[1;2A")),
    ("kUP3", Value::Str(r"\E[1;3A")),
    ("kUP4", Value::Str(r"\E[1;4A")),
    ("kUP5", Value::Str(r"\E[1;5A")),
    ("kUP6", Value::Str(r"\E[1;6A")),
    ("kUP7", Value::Str(r"\E[1;7A")),
    ("kbs", Value::Str(r"^?")),
    ("kcbt", Value::Str(r"\E[Z")),
    ("kcub1", Value::Str(r"\EOD")),
    ("kcud1", Value::Str(r"\EOB")),
    ("kcuf1", Value::Str(r"\EOC")),
    ("kcuu1", Value::Str(r"\EOA")),
    ("kdch1", Value::Str(r"\E[3~")),
    ("kend", Value::Str(r"\EOF")),
    ("kent", Value::Str(r"\EOM")),
    ("kf1", Value::Str(r"\EOP")),
    ("kf10", Value::Str(r"\E[21~")),
    ("kf11", Value::Str(r"\E[23~")),
    ("kf12", Value::Str(r"\E[24~")),
    ("kf13", Value::Str(r"\E[1;2P")),
    ("kf14", Value::Str(r"\E[1;2Q")),
    ("kf15", Value::Str(r"\E[1;2R")),
    ("kf16", Value::Str(r"\E[1;2S")),
    ("kf17", Value::Str(r"\E[15;2~")),
    ("kf18", Value::Str(r"\E[17;2~")),
    ("kf19", Value::Str(r"\E[18;2~")),
    ("kf2", Value::Str(r"\EOQ")),
    ("kf20", Value::Str(r"\E[19;2~")),
    ("kf21", Value::Str(r"\E[20;2~")),
    ("kf22", Value::Str(r"\E[21;2~")),
    ("kf23", Value::Str(r"\E[23;2~")),
    ("kf24", Value::Str(r"\E[24;2~")),
    ("kf25", Value::Str(r"\E[1;5P")),
    ("kf26", Value::Str(r"\E[1;5Q")),
    ("kf27", Value::Str(r"\E[1;5R")),
    ("kf28", Value::Str(r"\E[1;5S")),
    ("kf29", Value::Str(r"\E[15;5~")),
    ("kf3", Value::Str(r"\EOR")),
    ("kf30", Value::Str(r"\E[17;5~")),
    ("kf31", Value::Str(r"\E[18;5~")),
    ("kf32", Value::Str(r"\E[19;5~")),
    ("kf33", Value::Str(r"\E[20;5~")),
    ("kf34", Value::Str(r"\E[21;5~")),
    ("kf35", Value::Str(r"\E[23;5~")),
    ("kf36", Value::Str(r"\E[24;5~")),
    ("kf37", Value::Str(r"\E[1;6P")),
    ("kf38", Value::Str(r"\E[1;6Q")),
    ("kf39", Value::Str(r"\E[1;6R")),
    ("kf4", Value::Str(r"\EOS")),
    ("kf40", Value::Str(r"\E[1;6S")),
    ("kf41", Value::Str(r"\E[15;6~")),
    ("kf42", Value::Str(r"\E[17;6~")),
    ("kf43", Value::Str(r"\E[18;6~")),
    ("kf44", Value::Str(r"\E[19;6~")),
    ("kf45", Value::Str(r"\E[20;6~")),
    ("kf46", Value::Str(r"\E[21;6~")),
    ("kf47", Value::Str(r"\E[23;6~")),
    ("kf48", Value::Str(r"\E[24;6~")),
    ("kf49", Value::Str(r"\E[1;3P")),
    ("kf5", Value::Str(r"\E[15~")),
    ("kf50", Value::Str(r"\E[1;3Q")),
    ("kf51", Value::Str(r"\E[1;3R")),
    ("kf52", Value::Str(r"\E[1;3S")),
    ("kf53", Value::Str(r"\E[15;3~")),
    ("kf54", Value::Str(r"\E[17;3~")),
    ("kf55", Value::Str(r"\E[18;3~")),
    ("kf56", Value::Str(r"\E[19;3~")),
    ("kf57", Value::Str(r"\E[20;3~")),
    ("kf58", Value::Str(r"\E[21;3~")),
    ("kf59", Value::Str(r"\E[23;3~")),
    ("kf6", Value::Str(r"\E[17~")),
    ("kf60", Value::Str(r"\E[24;3~")),
    ("kf61", Value::Str(r"\E[1;4P")),
    ("kf62", Value::Str(r"\E[1;4Q")),
    ("kf63", Value::Str(r"\E[1;4R")),
    ("kf7", Value::Str(r"\E[18~")),
    ("kf8", Value::Str(r"\E[19~")),
    ("kf9", Value::Str(r"\E[20~")),
    ("khome", Value::Str(r"\EOH")),
    ("kich1", Value::Str(r"\E[2~")),
    ("kind", Value::Str(r"\E[1;2B")),
    ("kmous", Value::Str(r"\E[<")),
    ("knp", Value::Str(r"\E[6~")),
    ("kpp", Value::Str(r"\E[5~")),
    ("kri", Value::Str(r"\E[1;2A")),
    ("rs1", Value::Str(r"\E]\E\\\Ec")),
    ("sc", Value::Str(r"\E7")),
];

/// The entry in terminfo source format, ready for `tic`.
#[must_use]
pub fn source() -> String {
    let mut out = NAMES.join("|");
    out.push_str(",\n");
    for (name, value) in CAPABILITIES {
        out.push('\t');
        out.push_str(name);
        match value {
            Value::Canceled => out.push('@'),
            Value::Boolean => {}
            // Writing to a String cannot fail.
            Value::Numeric(v) => drop(write!(out, "#{v}")),
            Value::Str(v) => drop(write!(out, "={v}")),
        }
        out.push_str(",\n");
    }
    out
}

/// Where a test or a sandboxed run wants the database instead of `$HOME/.terminfo`.
pub const DIR_ENV: &str = "SLOPTY_TERMINFO_DIR";

/// The terminfo databases a program started by a shell would search, most specific first.
#[must_use]
pub fn dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os(DIR_ENV) {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".terminfo"));
    }
    if let Some(list) = std::env::var_os("TERMINFO_DIRS") {
        dirs.extend(std::env::split_paths(&list));
    }
    dirs.extend(
        ["/usr/share/terminfo", "/opt/homebrew/share/terminfo", "/usr/local/share/terminfo"]
            .map(PathBuf::from),
    );
    dirs
}

/// Whether a compiled `xterm-ghostty` entry is already on the system.
///
/// ncurses files an entry under the first letter of its name, or under that letter's hex code
/// on a case-insensitive filesystem — which is what macOS gives us by default.
#[must_use]
pub fn installed() -> bool {
    dirs()
        .iter()
        .any(|dir| dir.join("78/xterm-ghostty").exists() || dir.join("x/xterm-ghostty").exists())
}

/// Where `tic` writes: `$SLOPTY_TERMINFO_DIR` when a test or a sandboxed run names one, else
/// the user's own database — the first directory [`dirs`] searches either way.
#[must_use]
pub fn user_database() -> PathBuf {
    if let Some(dir) = std::env::var_os(DIR_ENV) {
        return PathBuf::from(dir);
    }
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from).join(".terminfo")
}

/// What [`install`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Installed {
    /// The entry was already compiled; nothing ran.
    Already,
    /// `tic` compiled the entry into the database.
    Compiled,
}

/// Compiling the entry failed.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// `tic` could not be run.
    #[error("{0}: {1}")]
    Spawn(&'static str, #[source] std::io::Error),
    /// `tic` ran and refused the entry.
    #[error("tic: {0}")]
    Tic(String),
}

/// The compiler macOS ships, never a `tic` from the user's `PATH`.
const TIC: &str = "/usr/bin/tic";

/// Compile [`source`] into `database` unless the entry is already there.
///
/// Idempotent, and safe to run while shells are starting: one that spawns before this finishes
/// is told `xterm-256color` by [`crate::pty::default_term`] and keeps working. `-x` keeps the
/// capabilities ncurses does not know, which here is most of the interesting ones.
///
/// # Errors
/// If `tic` is missing or rejects the entry.
pub async fn install(database: &std::path::Path) -> Result<Installed, InstallError> {
    if installed() {
        return Ok(Installed::Already);
    }
    tokio::fs::create_dir_all(database)
        .await
        .map_err(|e| InstallError::Spawn("create the terminfo database", e))?;
    let mut child = tokio::process::Command::new(TIC)
        .args(["-x", "-o"])
        .arg(database)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| InstallError::Spawn("run /usr/bin/tic", e))?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt as _;
        stdin
            .write_all(source().as_bytes())
            .await
            .map_err(|e| InstallError::Spawn("write the terminfo source", e))?;
        stdin.shutdown().await.map_err(|e| InstallError::Spawn("close tic's stdin", e))?;
    }
    let out = child.wait_with_output().await.map_err(|e| InstallError::Spawn("wait for tic", e))?;
    if out.status.success() {
        Ok(Installed::Compiled)
    } else {
        Err(InstallError::Tic(String::from_utf8_lossy(&out.stderr).trim().to_owned()))
    }
}
