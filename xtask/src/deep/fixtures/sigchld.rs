//! The `SIGCHLD` pump linked into every binary `cargo xtask deep sanitize thread` builds
//! (`docs/decisions/testing.md`, "Red means something", its `SIGCHLD` pump).
//!
//! `ThreadSanitizer` runs a signal's handler at once only on a thread inside a call it treats as
//! blocking; elsewhere it holds the signal until that thread next calls a function it
//! intercepts. The kernel hands a child's `SIGCHLD` to the first thread that does not block it,
//! which is the test harness's main thread, parked until its 60-second "has been running"
//! notice in a wait the runtime does not intercept (`dispatch_semaphore_wait` on macOS, a raw
//! `futex` on Linux). So every test that waits on a child through `SIGCHLD` waited a minute.
//!
//! Before `main`, this blocks `SIGCHLD` on the main thread, which every later thread inherits,
//! and starts one thread that waits in `sigsuspend` with nothing blocked: the only thread the
//! kernel can hand `SIGCHLD` to. `sigsuspend` returns through its interceptor, which runs the
//! handler the program installed, as the kernel would have. A child the program starts resets
//! its mask (`std::process` does, and so does `slopty-pty`'s own spawn).
//!
//! Built alone with `rustc --emit obj` and passed to the linker, so it is in every binary and
//! test whatever links it; `no_std` and free of panics, so the object needs nothing from a
//! standard library but the C calls below.

#![no_std]
#![allow(unsafe_code, reason = "a constructor and the C signal calls are the whole of it")]

use core::ffi::{c_int, c_void};

#[cfg(target_os = "macos")]
const SIGCHLD: c_int = 20;
#[cfg(target_os = "linux")]
const SIGCHLD: c_int = 17;
#[cfg(target_os = "macos")]
const SIG_BLOCK: c_int = 1;
#[cfg(target_os = "linux")]
const SIG_BLOCK: c_int = 0;

/// Room for either platform's `sigset_t` (4 bytes on macOS, 128 with glibc), filled only by
/// `sigemptyset` and `sigaddset`.
#[repr(C, align(8))]
struct SigSet([u8; 128]);

unsafe extern "C" {
    fn sigemptyset(set: *mut SigSet) -> c_int;
    fn sigaddset(set: *mut SigSet, sig: c_int) -> c_int;
    fn pthread_sigmask(how: c_int, set: *const SigSet, old: *mut SigSet) -> c_int;
    fn sigsuspend(mask: *const SigSet) -> c_int;
    fn pthread_create(
        thread: *mut usize,
        attr: *const c_void,
        start: extern "C" fn(*mut c_void) -> *mut c_void,
        arg: *mut c_void,
    ) -> c_int;
    fn pthread_detach(thread: usize) -> c_int;
}

/// The pump: the one thread with `SIGCHLD` unblocked, and only while it waits for a signal.
extern "C" fn pump(_: *mut c_void) -> *mut c_void {
    let mut nothing = SigSet([0; 128]);
    // SAFETY: `nothing` is a writable signal set at least as large as the platform's.
    unsafe { sigemptyset(&raw mut nothing) };
    loop {
        // SAFETY: as above; `sigsuspend` returns once a handler ran, and is called again.
        unsafe { sigsuspend(&raw const nothing) };
    }
}

/// Block `SIGCHLD` here, on the main thread before any other starts, and start the pump.
extern "C" fn start() {
    let mut child = SigSet([0; 128]);
    let mut thread = 0_usize;
    // SAFETY: the sets are writable and large enough; `thread` holds a `pthread_t` (a pointer
    // on macOS, an `unsigned long` on Linux), which the pump is detached by.
    unsafe {
        sigemptyset(&raw mut child);
        sigaddset(&raw mut child, SIGCHLD);
        pthread_sigmask(SIG_BLOCK, &raw const child, core::ptr::null_mut());
        if pthread_create(&raw mut thread, core::ptr::null(), pump, core::ptr::null_mut()) == 0 {
            pthread_detach(thread);
        }
    }
}

/// Run [`start`] before `main`, after the sanitizer's runtime has started.
#[used]
#[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__mod_init_func"))]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
static START: extern "C" fn() = start;
