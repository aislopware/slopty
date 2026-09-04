//! Posts real events; needs post-event (Accessibility) access, so it is gated by
//! `SLOPTY_INPUT_E2E=1` and skips itself when the process lacks the permission.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use objc2_core_graphics::CGMainDisplayID;
    use slopty_input::Injector;
    use slopty_proto::screen::{CaptureTarget, ScreenInput};

    /// Quarter of the way across and down the main display, in display points.
    const FRACTION: f64 = 0.25;

    #[expect(clippy::cast_possible_truncation, reason = "display points fit f32")]
    const fn to_f32(v: f64) -> f32 {
        v as f32
    }

    #[tokio::test]
    async fn moves_the_real_pointer_on_a_display_stream() {
        if std::env::var_os("SLOPTY_INPUT_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_INPUT_E2E=1");
            return;
        }
        if !slopty_input::can_post() {
            eprintln!("skipped: no post-event access");
            return;
        }
        let display = CGMainDisplayID();
        let bounds = slopty_capture::target_bounds(CaptureTarget::Display(display))
            .expect("main display has bounds");
        let (start_x, start_y) = slopty_capture::pointer_location();

        // A 1:1 stream, so stream pixels are display points.
        let mut injector = Injector::new(CaptureTarget::Display(display), 1.0);
        let (x, y) = (bounds.w * FRACTION, bounds.h * FRACTION);
        injector.inject(&ScreenInput::Move { x: to_f32(x), y: to_f32(y) }).expect("post");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (px, py) = slopty_capture::pointer_location();
        assert!(
            (px - (bounds.x + x)).abs() < 2.0 && (py - (bounds.y + y)).abs() < 2.0,
            "{px},{py}"
        );

        // Put it back.
        let back =
            ScreenInput::Move { x: to_f32(start_x - bounds.x), y: to_f32(start_y - bounds.y) };
        injector.inject(&back).expect("post");
    }
}
