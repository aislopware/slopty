//! `slopty-probe`, the developer's measurements against a worker. It is all in the library
//! (`slopty_cli::probe_main`).

#![forbid(unsafe_code)]

fn main() -> anyhow::Result<std::process::ExitCode> {
    slopty_proto::wire::set_commit(option_env!("SLOPTY_COMMIT"));
    slopty_cli::probe_main()
}
