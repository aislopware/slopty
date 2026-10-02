//! `slopty-worker`, the worker daemon. It is all in the library (`src/lib.rs`).

#![forbid(unsafe_code)]

fn main() -> anyhow::Result<std::process::ExitCode> {
    slopty_workerd::main()
}
