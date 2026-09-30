# block, vendored with one fix

The published `block` 0.1.6 (crates.io; unmaintained since 2016, so there is no upstream to wait
for), pulled into Slopty's graph by `cocoa`, `cocoa-foundation`, `metal` and `core-graphics2`
through gpui-fast.

`src/lib.rs` declared `enum Class { }` and then `static _NSConcreteStackBlock: Class;`. A static
of an uninhabited type is a future-incompatibility warning today and a hard error later
(rust#74840). `Class` is now `#[repr(C)] struct Class { _priv: [u8; 0] }`, opaque but inhabited.
The code only ever takes the statics' address (`isa: *const Class`), so behaviour is unchanged.

The `objc_test_utils` dev-dependency is dropped: its `test_utils` path is not in the published
crate, and nothing in Slopty runs `block`'s own tests.

To move it: delete this directory and its `[patch.crates-io]` line in the workspace `Cargo.toml`
once nothing in the graph depends on `block` (gpui-fast on objc2-metal, objc2-core-video, and
`gpui_macos` off cocoa; `.research/gpui-fast-objc2-migration.md`).
