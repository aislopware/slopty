//! zig as the C and C++ compiler of an iOS triple off a Mac.
//!
//! Clippy for the iOS triples never links, but build scripts compile C for the target (ring,
//! blake3). A Linux runner has no compiler for iOS, and `cargo-zigbuild`, which supplies zig for
//! macOS and Linux, refuses the iOS triples (run 38110275409). So [`crate::tools::apple_cargo`]
//! names this as `CC` and `CXX` for them, as links to xtask named for the compiler and target
//! ([`links`]): `cc` and sccache run a compiler with only their own arguments, so a `CC` with
//! words of its own lost them (run 38112204055). It drops what `cc` adds for Apple's clang
//! (`-arch`, `-isysroot` with an SDK no Linux runner has, the deployment target flags) and runs
//! `zig cc -target <zig triple>` with zig's bundled Darwin headers on the include path, which
//! zig takes for macOS on its own but not for iOS (run 38111307984: `assert.h` not found).

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};

/// The name a link to xtask starts with to be zig's C compiler for the target after it.
const CC: &str = "zigcc-";

/// The same for C++.
const CXX: &str = "zigcxx-";

/// When this process runs as a link from [`links`], it becomes that compiler: `Some` with what
/// went wrong if zig could not start, never `Some(Ok)`.
pub fn as_compiler() -> Option<Result<()>> {
    let mut argv = std::env::args();
    let name = argv.next()?;
    let name = Utf8Path::new(&name).file_name()?.to_owned();
    let (target, cxx) = match (name.strip_prefix(CC), name.strip_prefix(CXX)) {
        (Some(target), _) => (target.to_owned(), false),
        (_, Some(target)) => (target.to_owned(), true),
        _ => return None,
    };
    let args: Vec<String> = argv.collect();
    Some(run(&target, cxx, &args))
}

/// Links in `dir` to this xtask that run as zig's C and C++ compilers for `target` (zig's name:
/// `aarch64-ios`, `aarch64-ios-simulator`), made when missing.
pub fn links(dir: &Utf8Path, target: &str) -> Result<(Utf8PathBuf, Utf8PathBuf)> {
    let exe = std::env::current_exe().context("xtask's own path")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {dir}"))?;
    let cc = dir.join(format!("{CC}{target}"));
    let cxx = dir.join(format!("{CXX}{target}"));
    for link in [&cc, &cxx] {
        if std::fs::read_link(link).ok().as_deref() != Some(exe.as_path()) {
            let _gone = std::fs::remove_file(link);
            std::os::unix::fs::symlink(&exe, link).with_context(|| format!("link {link}"))?;
        }
    }
    Ok((cc, cxx))
}

/// The arguments `zig cc` gets for `args`, which `cc` wrote for Apple's clang: the target first,
/// then every argument but those naming Apple's SDK, architecture, target or deployment target.
pub fn arguments(target: &str, args: &[String]) -> Vec<String> {
    let mut kept = vec!["-target".to_owned(), target.to_owned()];
    let mut skip = false;
    for arg in args {
        if std::mem::take(&mut skip) {
            continue;
        }
        match arg.as_str() {
            "-isysroot" | "-arch" | "-target" => skip = true,
            a if a.starts_with("--target=")
                || a.starts_with("-isysroot")
                || (a.starts_with("-m") && a.contains("-version-min=")) => {}
            _ => kept.push(arg.clone()),
        }
    }
    kept
}

/// The directory of zig's library, from `zig env` (`.lib_dir = "…"`, or `"lib_dir": "…"` in
/// the JSON older zigs print).
pub fn lib_dir(env: &str) -> Option<&str> {
    let (_, rest) = env.split_once("lib_dir")?;
    // JSON's key ends in a quote of its own.
    let rest = rest.strip_prefix('"').unwrap_or(rest);
    let (_, rest) = rest.split_once('"')?;
    rest.split_once('"').map(|(dir, _)| dir)
}

/// Become `zig cc` (or `zig c++`) for `target` on `args`; returns only if zig could not start.
pub fn run(target: &str, cxx: bool, args: &[String]) -> Result<()> {
    use std::os::unix::process::CommandExt as _;

    let env = std::process::Command::new("zig").arg("env").output().context("zig env")?;
    let env = String::from_utf8_lossy(&env.stdout);
    let lib = lib_dir(&env).context("`zig env` names no lib_dir")?;
    let darwin = format!("{lib}/libc/include/any-darwin-any");
    let mut zig = std::process::Command::new("zig");
    zig.arg(if cxx { "c++" } else { "cc" });
    zig.args(["-isystem", &darwin]).args(arguments(target, args));
    // `cc` hands its SDK to the compiler as `SDKROOT` too, which zig would take for the sysroot.
    zig.env_remove("SDKROOT");
    Err(zig.exec()).context("run zig (is the zig vendor/ghostty names on PATH?)")
}

#[cfg(test)]
mod tests {
    use super::{arguments, lib_dir};

    #[test]
    fn zig_s_library_is_read_from_either_form_of_zig_env() {
        let zon = ".{\n    .zig_exe = \"/z/bin/zig\",\n    .lib_dir = \"/z/lib/zig\",\n}";
        assert_eq!(lib_dir(zon), Some("/z/lib/zig"));
        let json = "{\n \"zig_exe\": \"/z/bin/zig\",\n \"lib_dir\": \"/z/lib\"\n}";
        assert_eq!(lib_dir(json), Some("/z/lib"));
        assert_eq!(lib_dir("{}"), None);
    }

    #[test]
    fn apple_clang_s_target_flags_give_way_to_zig_s() {
        let given: Vec<String> = [
            "-O0",
            "-arch",
            "arm64",
            "-isysroot",
            "/stub/iPhoneOS.sdk",
            "--target=arm64-apple-ios",
            "-miphoneos-version-min=27.0",
            "-mios-simulator-version-min=27.0",
            "-I",
            "include",
            "-c",
            "a.c",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(
            arguments("aarch64-ios", &given),
            ["-target", "aarch64-ios", "-O0", "-I", "include", "-c", "a.c"]
        );
    }
}
