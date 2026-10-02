//! The app as `cargo xtask e2e` drives it: `slopty-app` itself, under a name only a build with
//! the self-test makes, so a plain build of the workspace's tests never replaces it.

#[path = "../main.rs"]
mod app;

fn main() -> anyhow::Result<()> {
    app::main()
}
