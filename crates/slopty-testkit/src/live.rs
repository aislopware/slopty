//! How a live test that cannot run here says so.
//!
//! One line, `skipped: <why>`, and the test returns. In the VM live lane (`cargo xtask vm live`,
//! which sets [`IN_VM`]) a guest lacks nothing a live test needs, so there the same call fails the
//! test instead of letting it pass unrun.

/// The variable `cargo xtask vm live` sets in its guest.
pub const IN_VM: &str = "SLOPTY_VM";

/// Say on standard error that the calling test is not running, and why; the caller returns next.
///
/// # Panics
///
/// In the VM live lane ([`IN_VM`] set), where a skip is a failure.
#[track_caller]
pub fn skip(why: &str) {
    skip_in(std::env::var_os(IN_VM).is_some(), why);
}

#[track_caller]
#[expect(
    clippy::print_stderr,
    reason = "a test helper: the skip is said where the test's output is"
)]
fn skip_in(in_vm: bool, why: &str) {
    assert!(
        !in_vm,
        "skipped in the VM live lane ({IN_VM} is set), where nothing is missing: {why}"
    );
    eprintln!("skipped: {why}");
}

#[cfg(test)]
mod tests {
    use super::skip_in;

    #[test]
    fn a_skip_outside_the_vm_lane_returns() {
        skip_in(false, "nothing to see");
    }

    #[test]
    #[should_panic(
        expected = "skipped in the VM live lane (SLOPTY_VM is set), where nothing is missing: fish"
    )]
    fn a_skip_in_the_vm_lane_fails_the_test() {
        skip_in(true, "fish");
    }
}
