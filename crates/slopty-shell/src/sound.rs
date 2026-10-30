// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Session notification sounds — the herdr approach (state-transition chimes
//! played through the platform's own audio CLI, zero Rust audio deps):
//!
//! - embedded completion, input-request and attention chimes;
//! - each cue writes to a temp file and uses the system player on a
//!   background thread: `afplay` (macOS), PowerShell `Media.SoundPlayer`
//!   (Windows), first of `paplay`/`pw-play`/`aplay`/`ffplay`/`mpv` (Linux —
//!   WAV, so even bare ALSA `aplay` decodes it);
//! - `SLOPTY_DISABLE_SOUND` env kill-switch + the `soundEnabled` ui-setting;
//! - failures are logged and swallowed — a missing player must never bother
//!   the session flow.
// slopty: voice and appshots removed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const DISABLE_ENV: &str = "SLOPTY_DISABLE_SOUND";
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

// slopty: voice and appshots removed.

static SOUND_DONE: &[u8] = include_bytes!("../assets/sounds/done.wav");
static SOUND_REQUEST: &[u8] = include_bytes!("../assets/sounds/request.wav");
static SOUND_ATTENTION: &[u8] = include_bytes!("../assets/sounds/attention.wav");

/// Which notification chime to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// An agent turn completed successfully.
    Done,
    /// The agent is waiting on a question (→ AwaitingInput).
    Request,
    /// A run failed or the durable connection state degraded.
    Attention,
}

/// Play a chime on a background thread. Silently a no-op when disabled or no
/// player is available.
pub fn play(sound: Sound) {
    let data = match sound {
        Sound::Done => SOUND_DONE,
        Sound::Request => SOUND_REQUEST,
        Sound::Attention => SOUND_ATTENTION,
    };
    play_in_background(data);
}

// slopty: voice and appshots removed.

fn play_in_background(data: &'static [u8]) {
    if std::env::var_os(DISABLE_ENV).is_some() {
        return;
    }
    std::thread::spawn(move || {
        if let Err(err) = play_bytes(data) {
            tracing::debug!(error = %err, "sound playback failed");
        }
    });
}

fn play_bytes(data: &[u8]) -> Result<(), String> {
    // The system players want a file path. Exclusive creation prevents a
    // predictable-name collision (including a pre-planted symlink) from
    // redirecting the embedded bytes to another file.
    let (mut file, tmp) = create_temp_file().map_err(|e| e.to_string())?;
    file.write_all(data).map_err(|e| e.to_string())?;
    drop(file);
    let result = run_player(&tmp.path);
    drop(tmp);
    result
}

struct TempSoundFile {
    path: PathBuf,
}

impl Drop for TempSoundFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn create_temp_file() -> std::io::Result<(std::fs::File, TempSoundFile)> {
    for _ in 0..128 {
        let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("slopty-sound-{}-{id}.wav", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((file, TempSoundFile { path })),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not reserve a notification sound file",
    ))
}

#[cfg(target_os = "macos")]
fn run_player(path: &Path) -> Result<(), String> {
    run_checked("afplay", &[], path)
}

#[cfg(windows)]
fn run_player(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // SoundPlayer handles WAV natively; PlaySync keeps the process alive for
    // the chime's duration. Pass the path through the child environment rather
    // than interpolating it into PowerShell source (paths may contain quotes).
    let script = "(New-Object Media.SoundPlayer $env:SLOPTY_SOUND_PATH).PlaySync()";
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .env("SLOPTY_SOUND_PATH", path)
        .creation_flags(0x08000000)
        .output()
        .map_err(|e| format!("powershell failed: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("powershell exited with {}", output.status))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn run_player(path: &Path) -> Result<(), String> {
    // WAV everywhere, so even bare ALSA aplay decodes it (herdr must exclude
    // aplay because it ships mp3s).
    let players: &[(&str, &[&str])] = &[
        ("paplay", &[]),
        ("pw-play", &[]),
        ("aplay", &["-q"]),
        ("ffplay", &["-nodisp", "-autoexit", "-loglevel", "quiet"]),
        ("mpv", &["--no-video", "--really-quiet"]),
    ];
    let mut errors = Vec::new();
    for (program, args) in players {
        match run_checked(program, args, path) {
            Ok(()) => return Ok(()),
            Err(err) => errors.push(err),
        }
    }
    Err(format!("no audio player available: {}", errors.join("; ")))
}

fn run_checked(program: &str, args: &[&str], path: &Path) -> Result<(), String> {
    // Bounded wait: a wedged audio daemon must not accumulate zombie threads.
    let mut child = std::process::Command::new(program)
        .args(args)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let deadline = crate::clock::stopwatch() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{program} exited with {status}")),
            Ok(None) => {
                if crate::clock::stopwatch() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{program} timed out"));
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program}: {err}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_files_are_exclusive_and_cleaned_up() {
        let (first_file, first) = create_temp_file().expect("reserve first temp file");
        let (second_file, second) = create_temp_file().expect("reserve second temp file");
        assert_ne!(first.path, second.path);
        assert!(first.path.exists());
        assert!(second.path.exists());
        drop(first_file);
        drop(second_file);
        let first_path = first.path.clone();
        let second_path = second.path.clone();
        drop(first);
        drop(second);
        assert!(!first_path.exists());
        assert!(!second_path.exists());
    }

    #[test]
    fn embedded_chimes_are_wav() {
        for data in [
            SOUND_DONE,
            SOUND_REQUEST,
            SOUND_ATTENTION, // slopty: voice and appshots removed.
        ] {
            assert!(data.len() > 1000);
            assert_eq!(&data[..4], b"RIFF");
            assert_eq!(&data[8..12], b"WAVE");
        }
    }
}
