//! The one test that makes a real virtual display. Creating it rearranges the Mac's screens
//! under whoever is using them, so it is ignored and also wants `SLOPTY_VDISPLAY_E2E=1`:
//! `SLOPTY_VDISPLAY_E2E=1 cargo nextest run -p slopty-vdisplay --run-ignored only`.

#[cfg(test)]
mod tests {
    use std::process::Command;

    #[test]
    #[ignore = "creates a display, rearranging the screens of whoever uses this Mac"]
    fn a_virtual_display_takes_its_mode_rotates_and_goes_away() {
        if std::env::var_os("SLOPTY_VDISPLAY_E2E").is_none_or(|v| v != "1") {
            slopty_testkit::live::skip("set SLOPTY_VDISPLAY_E2E=1 to create a display");
            return;
        }
        let out = Command::new(env!("CARGO_BIN_EXE_slopty-vdisplay-probe")).output().unwrap();
        let report = String::from_utf8_lossy(&out.stderr);
        eprintln!("{report}");
        assert!(out.status.success(), "{report}");
    }
}
