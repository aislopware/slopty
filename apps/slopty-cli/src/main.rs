//! `slopty`, the command line. It is all in the library (`src/lib.rs`).

#![forbid(unsafe_code)]

fn main() -> anyhow::Result<std::process::ExitCode> {
    slopty_proto::wire::set_commit(option_env!("SLOPTY_COMMIT"));
    slopty_cli::main()
}
