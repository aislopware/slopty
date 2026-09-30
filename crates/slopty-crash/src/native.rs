//! Fatal signals: a record written from the signal handler, and the report it becomes.
//!
//! The handler may only make async-signal-safe calls (`sigaction(2)` lists them): no allocation,
//! no locks, no symbolizer. It writes a short text record from a stack buffer with `open`,
//! `write` and `close`: what [`install`] prepared (the process, the build, where the executable
//! was loaded), then the signal, the fault address, the thread, the registers and the return
//! addresses up the frame-pointer chain, which the Apple arm64 ABI keeps in every function
//! that has a frame.
//!
//! [`finalize`] runs later, in the next process of the same build. The build UUID says the
//! code is the same, so a return address in the crashed executable resolves at the same offset
//! in this one's. A system library's resolves at the very same address while the dyld shared
//! cache sits where it sat, which holds until the machine restarts; `getpid`'s address, noted
//! in the record, says whether it still does.

use std::ffi::{CString, c_int, c_void};
use std::fmt::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use crate::image::{Image, Symbols, dl_image};
use crate::report::{Build, Frame, Kind, Report};
use crate::{Process, store};

/// The signals that end a process with a crash.
const SIGNALS: [c_int; 6] =
    [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE, libc::SIGABRT, libc::SIGTRAP];
/// The first line of every record, which also says its format.
const MAGIC: &str = "slopty-crash native 1";
/// Return addresses walked at most; a stack overflow's chain is as deep as the stack.
const MAX_FRAMES: usize = 128;
/// The user half of an arm64 address. A return address can carry a pointer-authentication
/// signature above it, which resolving must not see.
const ADDRESS_BITS: u64 = 0x0000_7fff_ffff_ffff;
/// How long a second crashing thread spins while the first one writes (about a second).
const WAIT_SPINS: u32 = 100_000_000;

/// No record written yet.
const IDLE: u8 = 0;
/// A thread is writing the record.
const WRITING: u8 = 1;
/// The record is written; a later signal in the same death adds nothing.
const WRITTEN: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(IDLE);
static WRITER: AtomicUsize = AtomicUsize::new(0);
static PREPARED: OnceLock<Prepared> = OnceLock::new();
static PREVIOUS: OnceLock<[(c_int, libc::sigaction); SIGNALS.len()]> = OnceLock::new();
/// The system id of the thread whose panic is about to abort the process, 0 while none is: set
/// by the panic hook only for core's second panic about one that could not unwind, which
/// aborts as soon as the hook returns. A caught panic never sets it.
static ABORTING_THREAD: AtomicU64 = AtomicU64::new(0);

/// What the handler writes without working it out: made once, at install.
struct Prepared {
    /// The crash directory.
    dir: CString,
    /// The process name, for the file name.
    process: &'static str,
    /// The record's first lines, which never change in this process.
    header: Vec<u8>,
}

/// Installs the handler for every signal in [`SIGNALS`], keeping each one's previous action to
/// hand the signal on to.
pub(crate) fn install(process: Process, dir: &Path, build: &Build) {
    let Ok(dir) = CString::new(dir.as_os_str().as_bytes()) else {
        return;
    };
    let mut header = format!("{MAGIC}\nprocess {}\n", process.name());
    if let Some(version) = &build.version {
        let _infallible = writeln!(header, "version {version}");
    }
    if let Some(image) = Image::current() {
        let _infallible = writeln!(
            header,
            "image {} {:#x} {:#x}",
            image.uuid_string(),
            image.base(),
            image.text()
        );
    }
    if let Some(anchor) = shared_cache_anchor() {
        let _infallible = writeln!(header, "anchor {anchor:#x}");
    }
    if let Some(exe) = &build.exe {
        let _infallible = writeln!(header, "exe {exe}");
    }
    if PREPARED.set(Prepared { dir, process: process.name(), header: header.into_bytes() }).is_err()
    {
        return;
    }

    let mut previous = [(0, empty_action()); SIGNALS.len()];
    for ((signal, old), wanted) in previous.iter_mut().zip(SIGNALS) {
        *signal = wanted;
        // SAFETY: `sigaction(2)` with a null new action only reads the current one into `old`,
        // which outlives the call.
        unsafe {
            libc::sigaction(wanted, std::ptr::null(), old);
        }
    }
    if PREVIOUS.set(previous).is_err() {
        return;
    }
    let handler: extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) = on_signal;
    let action = libc::sigaction {
        sa_sigaction: (handler as *const ()).expose_provenance(),
        sa_mask: 0,
        // `SA_ONSTACK`: a stack overflow's handler runs on the alternate stack Rust gives each
        // thread it starts, since the thread's own stack is spent.
        sa_flags: libc::SA_SIGINFO | libc::SA_ONSTACK,
    };
    for signal in SIGNALS {
        // SAFETY: `sigaction(2)` installs `on_signal`, an `extern "C"` function with the
        // `SA_SIGINFO` handler signature, which only makes async-signal-safe calls.
        unsafe {
            libc::sigaction(signal, &raw const action, std::ptr::null_mut());
        }
    }
}

/// Notes that the calling thread's panic ends in `abort()` right after the panic hook, so the
/// `SIGABRT` that follows belongs to that panic's report.
pub(crate) fn note_abort_follows() {
    ABORTING_THREAD.store(thread_id(), Ordering::Release);
}

/// Resolves, on a background thread, the records a previous run of this binary left for
/// `process`, if there are any. Costs one directory read when there are none.
pub(crate) fn finalize_in_background(process: Process, dir: &Path) {
    let pending = store::entries(dir)
        .iter()
        .any(|(_, _, _, name, ext)| ext == "native" && name == process.name());
    if !pending {
        return;
    }
    let dir = dir.to_path_buf();
    let _detached = std::thread::Builder::new().name("slopty-crash".to_owned()).spawn(move || {
        // SAFETY: `pthread_set_qos_class_self_np` (pthread/qos.h) changes only the calling
        // thread's class; a relative priority of 0 is within every class's range.
        unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
        }
        store::sweep(&dir);
        finalize(&dir);
    });
}

/// Turns every record in `dir` this build can resolve into a report, then keeps the directory
/// to its bound. A `SIGABRT` the handler saw follow a panic on its thread marks that panic's
/// report as aborted instead of adding one of its own.
///
/// Each record is claimed first, by renaming it to `<name>.<pid>`, so of two processes of one
/// build finalizing together exactly one resolves it. The symbolizer's caches are dropped
/// afterwards: a long-lived app would otherwise keep a whole binary's debug info in memory.
pub(crate) fn finalize(dir: &Path) {
    let Some(me) = Image::current() else {
        return;
    };
    let same_cache = shared_cache_anchor();
    let mut symbols = None;
    for (path, _, _, _, ext) in store::entries(dir) {
        if ext != "native" {
            continue;
        }
        let Some(record) = std::fs::read_to_string(&path).ok().as_deref().and_then(Record::parse)
        else {
            continue;
        };
        if record.image.as_ref().is_none_or(|image| image.uuid != me.uuid_string()) {
            continue;
        }
        let Some(claimed) = store::claim(&path) else {
            continue;
        };
        let same_cache = same_cache.is_some() && same_cache == record.anchor;
        let folded = record.signal == libc::SIGABRT
            && record.after_panic
            && mark_panic_aborted(dir, &record);
        let finished = if folded {
            Ok(())
        } else {
            let symbols = symbols.get_or_insert_with(|| me.symbols());
            let mut report = record.report(|address, is_return| {
                resolve(address, is_return, &record, &me, symbols.as_ref(), same_cache)
            });
            report.path = path.with_extension("json");
            store::rewrite(&report.path, &report)
        };
        if finished.is_ok() {
            let _gone = std::fs::remove_file(&claimed);
        } else {
            let _released = std::fs::rename(&claimed, &path);
        }
    }
    if symbols.is_some() {
        backtrace::clear_symbol_cache();
    }
    store::rotate(dir);
}

/// The record at `path` as a report whose frames are named by image offset only, for a listing
/// made before a run of its own build resolved it.
pub(crate) fn read_unresolved(path: &Path) -> Option<Report> {
    let record = Record::parse(&std::fs::read_to_string(path).ok()?)?;
    let mut report = record.report(|address, _| vec![record.raw_frame(address)]);
    path.clone_into(&mut report.path);
    Some(report)
}

/// Where `getpid` is in this process: the same in another process exactly while both map the
/// dyld shared cache at the same address.
fn shared_cache_anchor() -> Option<u64> {
    // SAFETY: `dlsym(3)` with `RTLD_DEFAULT` searches every loaded image for the NUL-terminated
    // name; it returns null when there is none.
    let anchor = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"getpid".as_ptr()) };
    (!anchor.is_null()).then(|| anchor.expose_provenance() as u64)
}

/// Marks the report of the panic that led to `record`'s abort: the latest report of the same
/// process and pid before it. Called only for a record whose handler saw that thread's panic
/// about to abort.
fn mark_panic_aborted(dir: &Path, record: &Record) -> bool {
    let panicked = store::entries(dir)
        .into_iter()
        .filter(|(_, time_ms, pid, process, ext)| {
            ext == "json"
                && *pid == record.pid
                && *process == record.process
                && *time_ms <= record.time_ms
        })
        .max_by_key(|(_, time_ms, ..)| *time_ms);
    let Some((path, ..)) = panicked else {
        return false;
    };
    let Some(mut report) = store::read(&path) else {
        return false;
    };
    let Kind::Panic { aborted, .. } = &mut report.kind else {
        return false;
    };
    *aborted = true;
    store::rewrite(&path, &report).is_ok()
}

/// The frames `address` is, resolved in this process.
fn resolve(
    address: u64,
    is_return: bool,
    record: &Record,
    me: &Image,
    symbols: Option<&Symbols>,
    same_cache: bool,
) -> Vec<Frame> {
    // A return address is the instruction after the call; the call is the one before it.
    let lookup = if is_return { address.saturating_sub(1) } else { address };
    if let Some(offset) = record.offset_in_image(lookup) {
        let local = usize::try_from(offset).ok().and_then(|o| me.base().checked_add(o));
        let mut frames = Vec::new();
        if let Some(local) = local.filter(|_| offset < me.text() as u64) {
            backtrace::resolve(std::ptr::with_exposed_provenance_mut(local), |symbol| {
                frames.push(Frame {
                    function: symbol.name().map(|name| format!("{name:#}")),
                    file: symbol.filename().map(|file| file.display().to_string()),
                    line: symbol.lineno(),
                    ..record.raw_frame(address)
                });
            });
        }
        if frames.is_empty() {
            frames.push(record.raw_frame(address));
        }
        // The debug info names a function without its path; the symbol table has it whole.
        if let (Some(symbols), Some(last)) = (symbols, frames.last_mut()) {
            last.function = symbols.function_at(offset).or_else(|| last.function.take());
        }
        return frames;
    }
    let found = same_cache.then(|| dl_image(usize::try_from(lookup).ok()?)).flatten();
    match found {
        Some((image, base, function)) => vec![Frame {
            address,
            image: Some(image),
            offset: address.checked_sub(base as u64),
            function,
            file: None,
            line: None,
        }],
        None => vec![record.raw_frame(address)],
    }
}

/// The crashed executable, as the record names it.
#[derive(Clone, PartialEq, Eq, Debug)]
struct RecordImage {
    uuid: String,
    base: u64,
    text: u64,
}

/// A parsed record.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct Record {
    process: String,
    version: Option<String>,
    exe: Option<String>,
    image: Option<RecordImage>,
    anchor: Option<u64>,
    pid: u32,
    time_ms: u64,
    signal: i32,
    address: u64,
    thread: Option<String>,
    /// The handler found this thread's panic about to abort ([`note_abort_follows`]).
    after_panic: bool,
    pc: u64,
    lr: u64,
    frames: Vec<u64>,
}

impl Record {
    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != MAGIC {
            return None;
        }
        let mut record = Self::default();
        for line in lines {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            match key {
                "process" => value.clone_into(&mut record.process),
                "version" => record.version = Some(value.to_owned()),
                "exe" => record.exe = Some(value.to_owned()),
                "image" => {
                    let mut parts = value.split(' ');
                    let (uuid, base, text) = (parts.next()?, parts.next()?, parts.next()?);
                    record.image = Some(RecordImage {
                        uuid: uuid.to_owned(),
                        base: hex(base)?,
                        text: hex(text)?,
                    });
                }
                "anchor" => record.anchor = hex(value),
                "pid" => record.pid = value.parse().ok()?,
                "time" => record.time_ms = value.parse().ok()?,
                "signal" => record.signal = value.parse().ok()?,
                "address" => record.address = hex(value)?,
                "thread" => record.thread = Some(value.to_owned()).filter(|t| !t.is_empty()),
                "pc" => record.pc = hex(value)?,
                "lr" => record.lr = hex(value)?,
                "frame" => record.frames.push(hex(value)?),
                "after_panic" => record.after_panic = value == "1",
                _ => {}
            }
        }
        (!record.process.is_empty() && record.pid != 0).then_some(record)
    }

    /// `address`'s offset in the crashed executable's code, if it is there.
    fn offset_in_image(&self, address: u64) -> Option<u64> {
        let image = self.image.as_ref()?;
        address.checked_sub(image.base).filter(|offset| *offset < image.text)
    }

    /// `address` named by image offset alone.
    fn raw_frame(&self, address: u64) -> Frame {
        let offset = self.offset_in_image(address);
        let image = offset.and(self.exe.as_deref()).map(|exe| {
            Path::new(exe)
                .file_name()
                .map_or_else(|| exe.to_owned(), |n| n.to_string_lossy().into_owned())
        });
        Frame { address, image, offset, ..Frame::default() }
    }

    /// The report, with each address's frames from `resolve(address, is_return)`.
    ///
    /// The link register is a frame only when it names a function neither the program counter
    /// nor the first return address does: a leaf function that made no frame record leaves its
    /// caller's return address there and nowhere else.
    fn report(&self, mut resolve: impl FnMut(u64, bool) -> Vec<Frame>) -> Report {
        let mut frames = resolve(self.pc, false);
        let walked: Vec<Vec<Frame>> =
            self.frames.iter().map(|address| resolve(*address, true)).collect();
        if self.lr != 0 {
            let caller = resolve(self.lr, true);
            let function = |frames: &[Frame]| frames.last().and_then(|f| f.function.clone());
            let named = function(&caller);
            if named.is_some()
                && named != function(&frames)
                && named != walked.first().and_then(|w| function(w))
            {
                frames.extend(caller);
            }
        }
        frames.extend(walked.into_iter().flatten());
        Report {
            process: self.process.clone(),
            pid: self.pid,
            time_ms: self.time_ms,
            thread: self.thread.clone(),
            kind: Kind::Signal {
                signal: self.signal,
                name: signal_name(self.signal).to_owned(),
                address: self.address,
            },
            frames,
            build: Build {
                version: self.version.clone(),
                exe: self.exe.clone(),
                uuid: self.image.as_ref().map(|image| image.uuid.clone()),
            },
            ips: None,
            path: std::path::PathBuf::new(),
        }
    }
}

fn hex(text: &str) -> Option<u64> {
    u64::from_str_radix(text.strip_prefix("0x")?, 16).ok()
}

/// `signal`'s name, for the signals the handler catches.
const fn signal_name(signal: c_int) -> &'static str {
    match signal {
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGBUS => "SIGBUS",
        libc::SIGILL => "SIGILL",
        libc::SIGFPE => "SIGFPE",
        libc::SIGABRT => "SIGABRT",
        libc::SIGTRAP => "SIGTRAP",
        _ => "signal",
    }
}

/// An empty action, to read the current one into.
const fn empty_action() -> libc::sigaction {
    libc::sigaction { sa_sigaction: libc::SIG_DFL, sa_mask: 0, sa_flags: 0 }
}

// --- The handler. Everything below runs inside it: async-signal-safe calls only. ---

/// The fatal-signal handler: writes the record once per process, then hands the signal on.
extern "C" fn on_signal(signal: c_int, info: *mut libc::siginfo_t, context: *mut c_void) {
    // SAFETY: `pthread_self` is async-signal-safe; it reads the thread's own pointer.
    let me = unsafe { libc::pthread_self() };
    match STATE.compare_exchange(IDLE, WRITING, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            WRITER.store(me, Ordering::Release);
            if let Some(prepared) = PREPARED.get() {
                write_record(prepared, signal, info, context);
            }
            STATE.store(WRITTEN, Ordering::Release);
        }
        // Another thread crashed first: let it finish before this one takes the process down.
        Err(WRITING) if WRITER.load(Ordering::Acquire) != me => {
            for _ in 0..WAIT_SPINS {
                if STATE.load(Ordering::Acquire) != WRITING {
                    break;
                }
                std::hint::spin_loop();
            }
        }
        Err(_) => {}
    }
    hand_on(signal, info, context);
}

/// Gives the signal to the handler that was there before (Rust's stack-overflow handler for
/// `SIGSEGV` and `SIGBUS`), or else restores the default action and raises it again, so the
/// process dies of it once this handler returns and `ReportCrash` sees it.
fn hand_on(signal: c_int, info: *mut libc::siginfo_t, context: *mut c_void) {
    let previous = PREVIOUS
        .get()
        .and_then(|all| all.iter().find(|(s, _)| *s == signal))
        .map(|(_, action)| *action);
    match previous {
        Some(action)
            if action.sa_sigaction != libc::SIG_DFL && action.sa_sigaction != libc::SIG_IGN =>
        {
            if action.sa_flags & libc::SA_SIGINFO != 0 {
                // SAFETY: with `SA_SIGINFO` set, the previous handler was installed as
                // `void (*)(int, siginfo_t *, void *)` (sigaction(2)), and it gets the arguments
                // this handler got.
                let handler = unsafe {
                    std::mem::transmute::<
                        libc::sighandler_t,
                        extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void),
                    >(action.sa_sigaction)
                };
                handler(signal, info, context);
            } else {
                // SAFETY: without `SA_SIGINFO`, the previous handler was installed as
                // `void (*)(int)` (sigaction(2)).
                let handler = unsafe {
                    std::mem::transmute::<libc::sighandler_t, extern "C" fn(c_int)>(
                        action.sa_sigaction,
                    )
                };
                handler(signal);
            }
        }
        _ => {
            let default = empty_action();
            // SAFETY: `sigaction` is async-signal-safe; this restores the default action.
            unsafe {
                libc::sigaction(signal, &raw const default, std::ptr::null_mut());
            }
            // SAFETY: `raise` is async-signal-safe. The signal is blocked while its handler runs,
            // so it stays pending and the default action takes it the moment this returns.
            unsafe {
                libc::raise(signal);
            }
        }
    }
}

/// Writes the record: the prepared header, then this crash.
fn write_record(
    prepared: &Prepared,
    signal: c_int,
    info: *const libc::siginfo_t,
    context: *const c_void,
) {
    let time_ms = now_ms();
    // SAFETY: `getpid` is async-signal-safe.
    let pid = unsafe { libc::getpid() };
    let pid = u64::try_from(pid).unwrap_or(0);

    let mut path = Buf::<1024>::new();
    path.push(prepared.dir.as_bytes());
    path.push(b"/");
    path.dec(time_ms);
    path.push(b"-");
    path.dec(pid);
    path.push(b"-");
    path.push(prepared.process.as_bytes());
    path.push(b".native\0");
    if path.overflowed {
        return;
    }
    // SAFETY: `mkdir` is async-signal-safe; the path is NUL-terminated. An existing directory
    // is the usual answer and not an error here.
    unsafe {
        libc::mkdir(prepared.dir.as_ptr(), 0o700);
    }
    // SAFETY: `open` is async-signal-safe; `path` ends in the NUL pushed above.
    let fd = unsafe {
        libc::open(
            path.bytes.as_ptr().cast(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return;
    }

    let mut out = Buf::<4096>::new();
    out.push(&prepared.header);
    line_dec(&mut out, b"pid ", pid);
    line_dec(&mut out, b"time ", time_ms);
    line_dec(&mut out, b"signal ", u64::try_from(signal).unwrap_or(0));
    let fault = if info.is_null() {
        0
    } else {
        // SAFETY: with `SA_SIGINFO` the kernel passes a valid `siginfo_t` (sigaction(2)).
        let address = unsafe { (*info).si_addr };
        address.addr() as u64
    };
    line_hex(&mut out, b"address ", fault);
    out.push(b"thread ");
    thread_name(&mut out);
    out.push(b"\n");
    let aborting = ABORTING_THREAD.load(Ordering::Acquire);
    if signal == libc::SIGABRT && aborting != 0 && aborting == thread_id() {
        out.push(b"after_panic 1\n");
    }

    let registers = registers(context);
    line_hex(&mut out, b"pc ", registers.pc);
    line_hex(&mut out, b"lr ", registers.lr & ADDRESS_BITS);
    let (low, high) = stack_bounds();
    let mut fp = registers.fp;
    for _ in 0..MAX_FRAMES {
        let Some(record) = frame_record(fp, low, high) else {
            break;
        };
        if record.1 == 0 {
            break;
        }
        if out.len > out.bytes.len().saturating_sub(64) {
            write_all(fd, out.filled());
            out.len = 0;
        }
        line_hex(&mut out, b"frame ", record.1 & ADDRESS_BITS);
        if record.0 <= fp {
            break;
        }
        fp = record.0;
    }
    write_all(fd, out.filled());
    // SAFETY: `close` is async-signal-safe; `fd` is the file opened above.
    unsafe {
        libc::close(fd);
    }
}

/// The interrupted thread's program counter, link register and frame pointer.
#[derive(Clone, Copy, Default)]
struct Registers {
    pc: u64,
    lr: u64,
    fp: u64,
}

/// `struct __darwin_ucontext`, `<sys/_types/_ucontext.h>`, up to `uc_mcontext` (each field is
/// the header's without its `uc_` prefix).
#[repr(C)]
struct UContext {
    onstack: c_int,
    sigmask: u32,
    stack: libc::stack_t,
    link: *mut c_void,
    mcsize: usize,
    mcontext: *const MContext64,
}

/// `struct __darwin_mcontext64` for arm64, `<arm/_mcontext.h>`, up to the thread state.
#[repr(C)]
struct MContext64 {
    /// `struct __darwin_arm_exception_state64`: `far`, `esr`, `exception`.
    es: [u64; 2],
    /// `struct __darwin_arm_thread_state64`.
    ss: ThreadState64,
}

/// `struct __darwin_arm_thread_state64`, `<mach/arm/_structs.h>`.
#[repr(C)]
struct ThreadState64 {
    x: [u64; 29],
    fp: u64,
    lr: u64,
    sp: u64,
    pc: u64,
    cpsr: u32,
    flags: u32,
}

#[cfg(target_arch = "aarch64")]
fn registers(context: *const c_void) -> Registers {
    if context.is_null() {
        return Registers::default();
    }
    // SAFETY: with `SA_SIGINFO` the third argument is the interrupted thread's `ucontext_t`
    // (sigaction(2)), laid out as `UContext` begins.
    let machine = unsafe { context.cast::<UContext>().read().mcontext };
    if machine.is_null() {
        return Registers::default();
    }
    // SAFETY: `uc_mcontext` points at the kernel-saved `__darwin_mcontext64`, alive for the
    // handler's duration; on arm64 its thread state is laid out as `ThreadState64`.
    let state = unsafe { &(*machine).ss };
    Registers { pc: state.pc & ADDRESS_BITS, lr: state.lr, fp: state.fp }
}

#[cfg(not(target_arch = "aarch64"))]
fn registers(_context: *const c_void) -> Registers {
    Registers::default()
}

/// The frame record at `fp`: the caller's frame pointer and the return address, if `fp` is an
/// aligned address inside this thread's stack.
fn frame_record(fp: u64, low: u64, high: u64) -> Option<(u64, u64)> {
    if fp < low || fp.checked_add(16)? > high || !fp.is_multiple_of(8) {
        return None;
    }
    let at = usize::try_from(fp).ok()?;
    // SAFETY: `fp` lies inside this thread's stack, which is mapped for the thread's life, and
    // is 8-aligned; the Apple arm64 ABI keeps a frame record (caller's x29, then x30) where
    // x29 points.
    let caller = unsafe { std::ptr::with_exposed_provenance::<u64>(at).read_volatile() };
    // SAFETY: as above; the return address is the record's second word, still inside the stack.
    let returns =
        unsafe { std::ptr::with_exposed_provenance::<u64>(at.checked_add(8)?).read_volatile() };
    Some((caller, returns))
}

/// This thread's stack, `[low, high)`.
fn stack_bounds() -> (u64, u64) {
    // SAFETY: `pthread_self` is async-signal-safe.
    let me = unsafe { libc::pthread_self() };
    // SAFETY: `pthread_get_stackaddr_np` reads the thread's own descriptor.
    let top = unsafe { libc::pthread_get_stackaddr_np(me) }.addr() as u64;
    // SAFETY: `pthread_get_stacksize_np` reads the thread's own descriptor.
    let size = unsafe { libc::pthread_get_stacksize_np(me) } as u64;
    (top.saturating_sub(size), top)
}

/// The thread's name, `main` for the main thread, else its id.
fn thread_name(out: &mut Buf<4096>) {
    let mut name = [0_u8; 64];
    // SAFETY: `pthread_self` is async-signal-safe.
    let me = unsafe { libc::pthread_self() };
    // SAFETY: `pthread_getname_np` copies at most `len` bytes of the thread's name,
    // NUL-terminated, into `name`; `me` is the calling thread, whose descriptor is alive.
    let got = unsafe { libc::pthread_getname_np(me, name.as_mut_ptr().cast(), name.len()) };
    let length = name.iter().position(|b| *b == 0).unwrap_or(name.len());
    let named = name.get(..length).filter(|n| got == 0 && !n.is_empty());
    if let Some(named) = named {
        // A name is the rest of its line: a newline in it would end it early.
        for byte in named {
            out.push(if *byte == b'\n' { b" " } else { std::slice::from_ref(byte) });
        }
        return;
    }
    // SAFETY: `pthread_main_np` reads the calling thread's own descriptor.
    if unsafe { libc::pthread_main_np() } != 0 {
        out.push(b"main");
        return;
    }
    out.push(b"thread ");
    out.dec(thread_id());
}

/// The calling thread's system-wide id.
fn thread_id() -> u64 {
    let mut id = 0_u64;
    // SAFETY: `pthread_threadid_np` with a null thread only reads the calling thread's own
    // descriptor and writes its id to `id`; it takes no lock, so the handler may call it.
    unsafe {
        libc::pthread_threadid_np(0, &raw mut id);
    }
    id
}

/// Milliseconds since the Unix epoch, from the async-signal-safe clock.
fn now_ms() -> u64 {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `clock_gettime` is async-signal-safe and writes `now`, which outlives the call.
    unsafe {
        libc::clock_gettime(libc::CLOCK_REALTIME, &raw mut now);
    }
    let seconds = u64::try_from(now.tv_sec).unwrap_or(0);
    let millis = u64::try_from(now.tv_nsec).unwrap_or(0) / 1_000_000;
    seconds.saturating_mul(1_000).saturating_add(millis)
}

fn line_dec(out: &mut Buf<4096>, key: &[u8], value: u64) {
    out.push(key);
    out.dec(value);
    out.push(b"\n");
}

fn line_hex(out: &mut Buf<4096>, key: &[u8], value: u64) {
    out.push(key);
    out.hex(value);
    out.push(b"\n");
}

fn write_all(fd: c_int, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: `write` is async-signal-safe; it reads at most `bytes.len()` bytes of `bytes`.
        let wrote = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        match usize::try_from(wrote) {
            Ok(0) => return,
            Ok(n) => bytes = bytes.get(n..).unwrap_or_default(),
            Err(_) if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {
            }
            Err(_) => return,
        }
    }
}

/// A fixed buffer on the handler's stack.
struct Buf<const N: usize> {
    bytes: [u8; N],
    len: usize,
    overflowed: bool,
}

impl<const N: usize> Buf<N> {
    const fn new() -> Self {
        Self { bytes: [0; N], len: 0, overflowed: false }
    }

    fn filled(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }

    fn push(&mut self, data: &[u8]) {
        let end = self.len.checked_add(data.len());
        match end.and_then(|end| Some((end, self.bytes.get_mut(self.len..end)?))) {
            Some((end, room)) => {
                room.copy_from_slice(data);
                self.len = end;
            }
            None => self.overflowed = true,
        }
    }

    fn dec(&mut self, mut value: u64) {
        let mut digits = [0_u8; 20];
        let mut used = 0_usize;
        for slot in digits.iter_mut().rev() {
            *slot = b"0123456789"
                .get(usize::try_from(value % 10).unwrap_or(0))
                .copied()
                .unwrap_or(b'?');
            value /= 10;
            used = used.saturating_add(1);
            if value == 0 {
                break;
            }
        }
        self.push(digits.get(digits.len().saturating_sub(used)..).unwrap_or_default());
    }

    fn hex(&mut self, value: u64) {
        let mut digits = *b"0x0000000000000000";
        for (slot, shift) in digits.iter_mut().skip(2).zip((0..64).step_by(4).rev()) {
            let nibble = value.checked_shr(shift).unwrap_or(0) & 0xf;
            *slot = b"0123456789abcdef"
                .get(usize::try_from(nibble).unwrap_or(0))
                .copied()
                .unwrap_or(b'?');
        }
        self.push(&digits);
    }
}

#[cfg(test)]
mod tests {
    use super::{Buf, MAGIC, Record, RecordImage, frame_record};
    use crate::report::Kind;

    #[test]
    fn numbers_format_without_allocating() {
        let mut buf = Buf::<64>::new();
        buf.dec(0);
        buf.push(b" ");
        buf.dec(u64::MAX);
        buf.push(b" ");
        buf.hex(0x1_0000_3f00);
        assert_eq!(
            std::str::from_utf8(buf.filled()).unwrap(),
            "0 18446744073709551615 0x0000000100003f00",
            "decimal and hex"
        );
        let mut small = Buf::<4>::new();
        small.push(b"hello");
        assert!(small.overflowed && small.len == 0, "an overflow writes nothing");
    }

    #[test]
    fn a_record_reads_back_and_names_frames_by_offset() {
        let text = format!(
            "{MAGIC}\nprocess slopty-worker\nversion 0.1.0\nimage AB-CD 0x100000000 0x10000\n\
             anchor 0x18c000000\nexe /opt/slopty bin/slopty-worker\npid 42\ntime 1790764058284\n\
             signal 11\naddress 0x0000000000000010\nthread tokio-runtime-worker\n\
             pc 0x0000000100001000\nlr 0x0000000100002000\nframe 0x0000000100003000\n\
             frame 0x000000018c001000\n"
        );
        let record = Record::parse(&text).unwrap();
        assert_eq!(record.exe.as_deref(), Some("/opt/slopty bin/slopty-worker"), "spaces kept");
        assert_eq!(
            record.image,
            Some(RecordImage { uuid: "AB-CD".to_owned(), base: 0x1_0000_0000, text: 0x10000 }),
            "image"
        );
        let report = record.report(|address, _| vec![record.raw_frame(address)]);
        assert_eq!(report.pid, 42, "pid");
        assert_eq!(report.thread.as_deref(), Some("tokio-runtime-worker"), "thread");
        assert_eq!(
            report.kind,
            Kind::Signal { signal: 11, name: "SIGSEGV".to_owned(), address: 0x10 },
            "kind"
        );
        let offsets: Vec<Option<u64>> = report.frames.iter().map(|f| f.offset).collect();
        assert_eq!(offsets, [Some(0x1000), Some(0x3000), None], "the unnamed lr is left out");
        assert_eq!(
            report.frames.first().and_then(|f| f.image.as_deref()),
            Some("slopty-worker"),
            "a frame in the executable names it"
        );
        assert!(Record::parse("slopty-crash native 0\n").is_none(), "another format");
        assert!(Record::parse(&format!("{MAGIC}\npid 1\n")).is_none(), "no process");
    }

    #[test]
    fn the_link_register_fills_in_a_leaf_functions_caller() {
        let record = Record {
            process: "slopty".to_owned(),
            pid: 1,
            pc: 1,
            lr: 2,
            frames: vec![3],
            ..Record::default()
        };
        let named = |address: u64, _| {
            vec![crate::report::Frame {
                address,
                function: Some(format!("f{address}")),
                ..crate::report::Frame::default()
            }]
        };
        let with_leaf: Vec<u64> = record.report(named).frames.iter().map(|f| f.address).collect();
        assert_eq!(with_leaf, [1, 2, 3], "the caller only the link register knows");
        let same = |address: u64, _| {
            vec![crate::report::Frame {
                address,
                function: Some(if address == 1 { "f1" } else { "g" }.to_owned()),
                ..crate::report::Frame::default()
            }]
        };
        let repeated: Vec<u64> = record.report(same).frames.iter().map(|f| f.address).collect();
        assert_eq!(
            repeated,
            [1, 3],
            "a link register naming the next frame's function adds nothing"
        );
    }

    #[test]
    fn the_walk_stays_inside_the_stack() {
        let words = [0_u64, 0xdead];
        let at = words.as_ptr().addr() as u64;
        assert_eq!(frame_record(at, at, at + 16), Some((0, 0xdead)), "inside");
        assert_eq!(frame_record(at, at + 8, at + 16), None, "below the stack");
        assert_eq!(frame_record(at, at, at + 8), None, "running off the top");
        assert_eq!(frame_record(at + 1, at, at + 32), None, "misaligned");
    }
}
