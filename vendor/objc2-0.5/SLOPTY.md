# objc2 0.5.2, vendored with one fix

The published `objc2` 0.5.2 (crates.io), which accesskit_macos still depends on (0.27.1, the
latest, asks for `^0.5.1`), with upstream's fix for
<https://github.com/madsmtm/objc2/issues/861> backported by hand into `src/rc/id.rs`. That fix
is <https://github.com/madsmtm/objc2/pull/862>, merged on 2026-09-25 for 0.6. `vendor/objc2`
carries the same fix for 0.6.4 and says why it matters. The backport makes the same two changes:
- each architecture's marker `asm!` takes the pointer as an input;
- the `nop` after `objc_retainAutoreleasedReturnValue` is emitted on every Apple target.

Together they mean the call can never be a tail call.

To move it: delete this directory and its `[patch.crates-io]` line once accesskit_macos moves to
objc2 0.6, or once a fixed 0.5 release is published.
