//! The panic hook: a report per panic, with its frames resolved on the spot.

use std::cell::RefCell;
use std::panic::PanicHookInfo;
use std::path::PathBuf;

use crate::Process;
use crate::report::{Build, Frame, Kind, Report};

/// Frames kept per report; the rest of a deep stack says nothing new.
const MAX_FRAMES: usize = 256;
/// The panic machinery's last frame: std marks it so its own short backtraces start after it.
const SHORT_BACKTRACE_END: &str = "__rust_end_short_backtrace";

/// What core says when a panic reaches a frame that cannot unwind, or a destructor panics
/// while unwinding (`core::panicking`): a second panic about the first, which then aborts.
const CONSEQUENCES: [&str; 2] =
    ["panic in a function that cannot unwind", "panic in a destructor during cleanup"];

thread_local! {
    /// This thread's last report, which a panic about it amends instead of adding its own.
    static LAST: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Puts the report writer in front of the current panic hook.
pub(crate) fn install(process: Process, dir: PathBuf, build: Build) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let consequence = info.payload_as_str().is_some_and(|m| CONSEQUENCES.contains(&m));
        #[cfg(target_vendor = "apple")]
        if consequence {
            crate::native::note_abort_follows();
        }
        if !(consequence && amend_last()) {
            let written = crate::store::write_new(&dir, &report(process, &build, info)).ok();
            // A thread whose locals are gone (a panic in a TLS destructor) keeps no last report.
            let _kept = LAST.try_with(|last| last.replace(written));
            // A caught panic leaves the process running: its symbolizer caches go with it.
            backtrace::clear_symbol_cache();
        }
        previous(info);
    }));
}

/// Marks this thread's last report aborted, for core's panic about it, which ends the process;
/// says whether it did.
fn amend_last() -> bool {
    let Some(path) = LAST.try_with(|last| last.borrow().clone()).ok().flatten() else {
        return false;
    };
    let Some(mut report) = crate::store::read(&path) else {
        return false;
    };
    if let Kind::Panic { aborted, .. } = &mut report.kind {
        *aborted = true;
    }
    crate::store::rewrite(&path, &report).is_ok()
}

/// The report of the panic `info` describes, made on the panicking thread.
fn report(process: Process, build: &Build, info: &PanicHookInfo<'_>) -> Report {
    let message = info.payload_as_str().unwrap_or("a panic with a non-string payload").to_owned();
    let location = info.location().map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()));
    let thread = std::thread::current();
    let thread = thread.name().map_or_else(|| format!("{:?}", thread.id()), str::to_owned);
    Report {
        process: process.name().to_owned(),
        pid: std::process::id(),
        time_ms: crate::time::now_ms(),
        thread: Some(thread),
        kind: Kind::Panic { message, location, aborted: false },
        frames: frames_here(),
        build: build.clone(),
        ips: None,
        path: PathBuf::new(),
    }
}

/// This thread's frames from the code that panicked outwards.
fn frames_here() -> Vec<Frame> {
    let trace = backtrace::Backtrace::new();
    #[cfg(target_vendor = "apple")]
    let own = crate::image::Image::current().and_then(|image| Some((image, image.symbols()?)));
    let mut frames = Vec::new();
    for frame in trace.frames() {
        let address = frame.ip().addr();
        let module = frame.module_base_address().map(<*mut std::ffi::c_void>::addr);
        let (image, base) = image_of(address, module);
        let raw = Frame {
            address: address as u64,
            offset: base.and_then(|base| address.checked_sub(base)).map(|o| o as u64),
            image,
            ..Frame::default()
        };
        if frame.symbols().is_empty() {
            frames.push(raw.clone());
        }
        for symbol in frame.symbols() {
            frames.push(Frame {
                function: symbol.name().map(|name| format!("{name:#}")),
                file: symbol.filename().map(|file| file.display().to_string()),
                line: symbol.lineno(),
                ..raw.clone()
            });
        }
        // The debug info names a function without its path; the symbol table has it whole.
        // A return address follows its call, so the call is looked up.
        #[cfg(target_vendor = "apple")]
        if let Some((image, symbols)) = &own {
            let call = address.saturating_sub(1);
            let offset = call.checked_sub(image.base()).filter(|o| *o < image.text());
            if let (Some(offset), Some(last)) = (offset, frames.last_mut()) {
                last.function = symbols.function_at(offset as u64).or_else(|| last.function.take());
            }
        }
    }
    trim(&mut frames);
    frames.truncate(MAX_FRAMES);
    frames
}

/// Drops the frames of the hook and the panic machinery above the code that panicked.
fn trim(frames: &mut Vec<Frame>) {
    let named = |frame: &Frame, prefix: &str| {
        frame.function.as_deref().is_some_and(|function| function.contains(prefix))
    };
    let end = frames.iter().position(|frame| named(frame, SHORT_BACKTRACE_END));
    let machinery = [
        "backtrace::",
        "slopty_crash::",
        "std::panicking::",
        "core::panicking::",
        "rust_begin_unwind",
        "std::panic::",
        "<alloc::boxed::Box<F,A> as core::ops::function::Fn<Args>>::call",
    ];
    let first = end.map_or_else(
        || frames.iter().position(|f| !machinery.iter().any(|m| named(f, m))).unwrap_or(0),
        |end| end.saturating_add(1),
    );
    frames.drain(..first.min(frames.len()));
    let still_machinery =
        |f: &Frame| ["rust_begin_unwind", "core::panicking::"].iter().any(|m| named(f, m));
    let first = frames.iter().position(|f| !still_machinery(f)).unwrap_or(0);
    frames.drain(..first);
}

/// The file name of the image holding `address`, and where it is loaded.
#[cfg(target_vendor = "apple")]
fn image_of(address: usize, module_base: Option<usize>) -> (Option<String>, Option<usize>) {
    crate::image::dl_image(address)
        .map_or((None, module_base), |(name, base, _)| (Some(name), Some(base)))
}

/// Where the image holding a frame is loaded; its name is not known here.
#[cfg(not(target_vendor = "apple"))]
const fn image_of(_address: usize, module_base: Option<usize>) -> (Option<String>, Option<usize>) {
    (None, module_base)
}

#[cfg(test)]
mod tests {
    use super::{Frame, trim};

    fn named(function: &str) -> Frame {
        Frame { function: Some(function.to_owned()), ..Frame::default() }
    }

    #[test]
    fn the_report_starts_at_the_code_that_panicked() {
        let mut frames = vec![
            named("backtrace::backtrace::trace"),
            named("slopty_crash::panic::frames_here"),
            named("std::panicking::panic_with_hook"),
            named("std::sys::backtrace::__rust_end_short_backtrace"),
            named("__rustc::rust_begin_unwind"),
            named("core::panicking::panic_fmt"),
            named("core::option::unwrap_failed"),
            named("slopty_worker::serve"),
        ];
        trim(&mut frames);
        let names: Vec<&str> = frames.iter().filter_map(|f| f.function.as_deref()).collect();
        assert_eq!(names, ["core::option::unwrap_failed", "slopty_worker::serve"], "trimmed");
    }

    #[test]
    fn without_the_marker_the_hook_frames_still_go() {
        let mut frames = vec![
            named("backtrace::capture::Backtrace::new"),
            named("slopty_crash::panic::report"),
            named("my_app::run"),
        ];
        trim(&mut frames);
        assert_eq!(frames.len(), 1, "only the app's frame: {frames:?}");
    }
}
