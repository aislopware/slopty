//! A stand-in for `claude`, for tests that start Claude Code without spending anyone's plan
//! (`docs/decisions/agents.md`, "The stand-in").
//!
//! Slopty runs Claude Code only driven, over stream-json with the control protocol, and so does
//! this: `-p --input-format stream-json …` is served by [`slopty_testkit::claude_stream`], whose
//! module says what it answers, the turns it plays and the failures it scripts. Besides, it
//! answers `--version` as Claude Code does, so a worker lists it as installed, and refuses
//! `--managed-help` as Claude Code refuses it: it is no managed launcher. Run any other way, it
//! says so and exits 2.

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "a command-line program")]

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("{} (Claude Code)", slopty_testkit::claude_stream::VERSION);
        return ExitCode::SUCCESS;
    }
    if args.first().is_some_and(|a| a == "--managed-help") {
        eprintln!("error: unknown option '--managed-help'");
        return ExitCode::FAILURE;
    }
    if slopty_testkit::claude_stream::wanted(&args) {
        return slopty_testkit::claude_stream::run(&args);
    }
    eprintln!("slopty-stub-claude runs only driven: -p --input-format stream-json");
    ExitCode::from(2)
}
