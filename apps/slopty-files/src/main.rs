//! The File Provider extension's executable: the system starts it inside
//! `Slopty.app/Contents/PlugIns/SloptyFiles.appex` (`slopty_files::extension`).

use std::process::ExitCode;

#[cfg(target_os = "macos")]
fn main() -> ExitCode {
    ExitCode::from(u8::try_from(slopty_files::extension::main()).unwrap_or(u8::MAX))
}

#[cfg(not(target_os = "macos"))]
#[expect(clippy::print_stderr, reason = "the one word for someone who runs it elsewhere")]
fn main() -> ExitCode {
    eprintln!("slopty-files is the Mac app's File Provider extension");
    ExitCode::FAILURE
}
