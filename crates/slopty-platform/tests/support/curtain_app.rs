//! An application that puts a magenta window over the main display and a curtain's shield
//! over every display, for `tests/curtain.rs`, which is its parent and reads the screens through
//! ScreenCaptureKit with and without the shield left out.
//!
//! It makes the shield's windows first and keeps them off the screens, says
//! `made test=<number> shield=<number>,<number>…`, puts the shield up on a `show` line (`shown`),
//! takes it down on a `hide` line (`hidden`), and leaves when its stdin closes. A test app, never
//! installed.

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod macos {
    use std::io::{BufRead as _, Write as _};
    use std::process::ExitCode;
    use std::sync::mpsc;

    use objc2::{MainThreadMarker, MainThreadOnly as _};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSScreen,
        NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSDate, NSRunLoop};
    use slopty_platform::curtain::Shield;

    fn say(line: &str) {
        let mut out = std::io::stdout().lock();
        let _written = writeln!(out, "{line}").and_then(|()| out.flush());
    }

    pub fn run() -> ExitCode {
        let Some(mtm) = MainThreadMarker::new() else { return ExitCode::FAILURE };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
        let Some(screen) = NSScreen::mainScreen(mtm) else { return ExitCode::FAILURE };
        // SAFETY: a borderless window over the main screen, made now; `NSWindow`'s designated
        // initialiser with valid arguments.
        let test = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                screen.frame(),
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: the window is owned here, so `close` must not release it.
        unsafe {
            test.setReleasedWhenClosed(false);
        }
        test.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            1.0, 0.0, 1.0, 1.0,
        )));
        test.orderFrontRegardless();
        let mut shield = Some(Shield::new(mtm, &|_display| false));
        let ids = shield.as_ref().map(Shield::window_ids).unwrap_or_default();
        let ids: Vec<String> = ids.iter().map(u32::to_string).collect();
        say(&format!("made test={} shield={}", test.windowNumber(), ids.join(",")));

        let (lines_tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                if lines_tx.send(line).is_err() {
                    return;
                }
            }
        });
        loop {
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.02));
            match lines.try_recv() {
                Ok(line) if line == "show" => {
                    if let Some(shield) = shield.as_mut() {
                        shield.show();
                    }
                    say("shown");
                }
                Ok(line) if line == "hide" => {
                    drop(shield.take());
                    say("hidden");
                }
                Ok(_) | Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        drop(shield);
        test.close();
        ExitCode::SUCCESS
    }
}
