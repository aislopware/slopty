# objc2, vendored with one fix

The published `objc2` 0.6.4 (crates.io) with the source part of upstream's fix for
<https://github.com/madsmtm/objc2/issues/861>, merged as
<https://github.com/madsmtm/objc2/pull/862> on 2026-09-25 and not yet released.

`Retained::retain_autoreleased`, which every `msg_send!` returning an autoreleased object goes
through, could be tail-called once optimised. On arm64 the macOS 13+ runtime's
`objc_retainAutoreleasedReturnValue` reads its return address to decide what to retain, so a
tail call could make it retain the wrong object: a use-after-free. Slopty builds every dependency
at opt-level 3, so debug builds are exposed as well as release builds. The fix passes the pointer
into the marker `asm!` and emits it on every Apple target, so the call can never be a tail call.

Only the four hunks in `src/rc/retained.rs` outside its tests are applied. Upstream's test
additions (`AUTORELEASE_SKIPPED` in `rc/test_object.rs` and the `__macros` tests) are left out,
because they do not apply to 0.6.4's test layout and nothing in Slopty runs objc2's own tests.

To move it: when objc2 0.6.5 or later is published with the fix, delete this directory and its
`[patch.crates-io]` line in the workspace `Cargo.toml`, and drop the `=0.6.4` pin.
