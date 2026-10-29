//! A stand-in for `claude`, for tests that start Claude Code without spending anyone's plan
//! (`docs/decisions/agents.md`, "TUI-first").
//!
//! It is Claude Code's TUI as Slopty meets it in a terminal ([`slopty_testkit::claude_tui`], whose
//! module says what it records). Besides, it answers `--version` as Claude Code does, so a worker
//! lists it as installed, and refuses `--managed-help` as Claude Code refuses it: it is no
//! managed launcher.

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "a command-line program")]

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("{} (Claude Code)", slopty_testkit::claude_tui::VERSION);
        return ExitCode::SUCCESS;
    }
    if args.first().is_some_and(|a| a == "--managed-help") {
        eprintln!("error: unknown option '--managed-help'");
        return ExitCode::FAILURE;
    }
    slopty_testkit::claude_tui::run(&args)
}
