//! What each fuzz target does with its input, as plain functions.
//!
//! Every decoder a peer's bytes reach has a target: the control and unidirectional streams
//! through the real framing ([`slopty_proto::codec::try_take`], as `slopty_net::framed` reads
//! them), each kind of datagram, the media reassembler and its forward error correction, the
//! worker's side of a client's loss feedback, a video frame's NAL unit framing and SPS reads,
//! the worker's control socket and its terminal engine. So has everything an agent's own
//! records and words reach: a Claude Code transcript ([`transcript`]), its hook payloads
//! ([`hooks`]) and its record of itself ([`registry`]), the markdown an answer is drawn from
//! ([`markdown`]) and a visual's script, assets and shaders ([`visual`]).
//!
//! `fuzz_targets/` hands each function an engine's input through [`target!`]: libFuzzer's under
//! the `libfuzzer` feature, AFL++'s through ziggy under `ziggy`, and with neither the files a
//! plain build is given on its command line. `tests/regressions.rs` replays through
//! the same function, on an ordinary build, every input kept under `regressions/<target>/` (a
//! crash found and fixed) and the target's committed corpus under `corpus/<target>/` (the
//! minimal set of inputs that reach all the code the fuzzers have reached), so a fixed crash
//! stays fixed and what the fuzzers covered stays held.
//!
//! A target fails by panicking: on a crash in the code under test, or on an invariant the
//! decoded value breaks (it must encode back to the same bytes, and the fast paths that read a
//! message's head must agree with the full decode). A decode must also take no more heap than
//! its bytes bound, plus the cells its terminal lines took from the decode's cell budget
//! ([`stream`]'s `decode`), counted by the allocator the decoding targets install.

pub mod datagram;
pub mod feedback;
pub mod hooks;
pub mod markdown;
pub mod nal;
pub mod reassemble;
pub mod registry;
pub mod stream;
pub mod terminal;
pub mod transcript;
pub mod visual;

/// One fuzz target: its name (the file under `fuzz_targets/` and its directory under
/// `regressions/`) and what it does with an input.
pub type Target = (&'static str, fn(&[u8]));

/// Every target.
pub const TARGETS: &[Target] = &[
    ("client_msg", stream::client_msg),
    ("worker_msg", stream::worker_msg),
    ("server_msg", stream::server_msg),
    ("uni_stream", stream::uni_stream),
    ("client_datagram", datagram::client_datagram),
    ("term_datagram", datagram::term_datagram),
    ("media_header", datagram::media_header),
    ("cursor", datagram::cursor),
    ("origin", datagram::origin),
    ("reassemble", reassemble::run),
    ("feedback", feedback::run),
    ("ctl", stream::ctl),
    ("nal", nal::run),
    ("terminal", terminal::run),
    ("transcript", transcript::run),
    ("hooks", hooks::run),
    ("registry", registry::run),
    ("markdown", markdown::run),
    ("visual", visual::run),
];

/// Let a panic raised in the sources of `crates` (pieces of a path, such as `/gltf-1.`) unwind
/// to the code under test, which catches it.
///
/// Such a panic is an upstream reader's that asserts on malformed input rather than failing,
/// which Slopty contains with `catch_unwind`. Any other panic goes on to the hook in place
/// before, which aborts under libFuzzer and AFL++ (and on a plain build prints it), and a
/// contained one that escapes still ends the run where it leaves the engine's entry. Called from
/// a target's `init` ([`target!`]), once the engine's hook is in place.
pub fn contain_panics_in(crates: &'static [&'static str]) {
    let abort = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let file = info.location().map(std::panic::Location::file).unwrap_or_default();
        if !crates.iter().any(|piece| file.contains(piece)) {
            abort(info);
        }
    }));
}

/// The target named `name`.
#[must_use]
pub fn target(name: &str) -> Option<fn(&[u8])> {
    TARGETS.iter().find(|(n, _)| *n == name).map(|(_, run)| *run)
}

#[cfg(all(feature = "libfuzzer", feature = "ziggy"))]
compile_error!(
    "the `libfuzzer` and `ziggy` features each build the targets' entry points: pick one"
);

/// [`target!`] as the `libfuzzer` build has it: cargo-fuzz's `fuzz_target!`.
#[cfg(feature = "libfuzzer")]
#[macro_export]
macro_rules! target {
    (init: $init:expr, |$data:ident| $body:expr) => {
        ::libfuzzer_sys::fuzz_target!(init: $init, |$data: &[u8]| $body);
    };
    (|$data:ident| $body:expr) => {
        ::libfuzzer_sys::fuzz_target!(|$data: &[u8]| $body);
    };
}

/// [`target!`] as the `ziggy` build has it: ziggy's `main`, AFL++'s persistent loop under
/// `cargo ziggy`, with `init` run on the first input, after afl.rs has set its panic hook.
#[cfg(all(feature = "ziggy", not(feature = "libfuzzer")))]
#[macro_export]
macro_rules! target {
    (init: $init:expr, | $data:ident | $body:expr) => {
        fn main() {
            static INIT: ::std::sync::Once = ::std::sync::Once::new();
            ::ziggy::fuzz!(|$data: &[u8]| {
                INIT.call_once(|| $init);
                $body
            });
        }
    };
    (| $data:ident | $body:expr) => {
        fn main() {
            ::ziggy::fuzz!(|$data: &[u8]| { $body });
        }
    };
}

/// A `fuzz_targets/` entry point that runs `body` on each input, after `init` once when given.
///
/// It is written `slopty_fuzz::target!(|data| body)` or
/// `slopty_fuzz::target!(init: expr, |data| body)`, in a file whose root carries
/// `#![cfg_attr(feature = "libfuzzer", no_main)]`. The feature picks the engine:
/// - `libfuzzer` (`cargo xtask fuzz` builds with it): cargo-fuzz's `fuzz_target!`;
/// - `ziggy` (`cargo xtask fuzz --ziggy`; `cargo ziggy` turns it on through `ziggy/afl`): ziggy's
///   `main`, AFL++'s persistent loop;
/// - neither, as here and as the gate builds and lints the targets: a `main` that runs every file
///   named on its command line ([`run_files`]).
#[cfg(not(any(feature = "libfuzzer", feature = "ziggy")))]
#[macro_export]
macro_rules! target {
    (init: $init:expr, | $data:ident | $body:expr) => {
        fn main() {
            $init;
            $crate::run_files(|$data: &[u8]| $body);
        }
    };
    (| $data:ident | $body:expr) => {
        fn main() {
            $crate::run_files(|$data: &[u8]| $body);
        }
    };
}

/// Every file named on the command line through `run`, in order: a target's binary on a plain
/// build (`cargo run --manifest-path fuzz/Cargo.toml --bin <target> -- <file>…`).
pub fn run_files(run: impl Fn(&[u8])) {
    for name in std::env::args_os().skip(1) {
        let path = std::path::Path::new(&name);
        let input =
            std::fs::read(path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
        run(&input);
    }
}
