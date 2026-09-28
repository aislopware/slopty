//! Allocation budgets on the terminal's echo path, counted by `slopty_testkit::alloc` on the
//! test's own thread: a keystroke's byte into the engine, the diff frame it makes, that frame
//! encoded for the wire, and the one encoding handed to many viewers. The counts are exact and
//! do not depend on the machine's load, so they gate. `docs/decisions/testing.md`, "Allocation
//! budgets".

#[cfg(test)]
#[expect(clippy::arithmetic_side_effects, reason = "byte counts and rows of small screens")]
mod allocs {
    use slopty_engine::{EngineConfig, GhosttyEngine};
    use slopty_proto::codec;
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::{Frame, TermEvent, TermSize};
    use slopty_testkit::alloc::{self, Allocs, Counting};

    #[global_allocator]
    static ALLOC: Counting = Counting;

    /// The sizes every budget holds on: the bench's terminal and a full-screen one.
    const SIZES: [(u16, u16); 2] = [(80, 24), (200, 60)];

    fn prompt() -> &'static [u8] {
        b"\x1b]133;D;0\x07\x1b]133;A\x07\x1b[1m~/src/slopty\x1b[0m on \x1b[35mmain\x1b[0m % \x1b]133;B\x07"
    }

    /// An engine whose screen is full of `ls -l`-like output with the cursor at a prompt on the
    /// bottom row, its attach frame taken and encoded and a few echoes behind it, so every buffer
    /// the echo path reuses has grown, the encoder's scratch buffer on this thread among them.
    fn at_a_full_prompt(cols: u16, rows: u16) -> GhosttyEngine {
        let size = TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } };
        let mut e = GhosttyEngine::new(EngineConfig { size, scrollback_lines: 10_000 }).unwrap();
        for i in 0..u32::from(rows) * 2 {
            let line = format!(
                "-rw-r--r--  1 cong  staff  {:>6} Sep 25 09:{:02} \x1b[34mfile-{i}.rs\x1b[0m\r\n",
                i * 37,
                i % 60
            );
            e.write(line.as_bytes());
        }
        e.write(prompt());
        let _attach = codec::encode(&TermEvent::Frame(e.full_frame(0).unwrap())).unwrap();
        for seq in 1..=8 {
            e.write(b"x");
            let _echo = e.take_frame(seq).unwrap();
        }
        e.write(b"\x08\x08\x08\x08\x08\x08\x08\x08\x1b[K");
        let _erased = e.take_frame(9).unwrap();
        e
    }

    /// One echoed key: the byte into the engine, then its diff frame.
    fn echo(e: &mut GhosttyEngine, seq: u64) -> (Allocs, Allocs, Frame) {
        let ((), write) = alloc::measure(|| e.write(b"l"));
        let (frame, take) = alloc::measure(|| e.take_frame(seq).unwrap());
        (write, take, frame.expect("an echo dirties its row"))
    }

    /// A keystroke's echo costs the same few blocks on any screen: the byte is fed in place, and
    /// the diff carries the one row it changed, never a read of the whole screen.
    #[test]
    fn an_echo_allocates_a_fixed_few_blocks_on_any_screen() {
        assert!(alloc::installed(), "the counting allocator is this binary's");
        let mut takes = Vec::new();
        for (cols, rows) in SIZES {
            let mut e = at_a_full_prompt(cols, rows);
            let (write, take, frame) = echo(&mut e, 10);
            eprintln!(
                "{cols}x{rows}: write {write}; take_frame {take}; {} rows",
                frame.updates.len()
            );
            assert_eq!(frame.updates.len(), 1, "the echo's row only");
            assert_eq!(write.blocks, 0, "{cols}x{rows}: a byte is fed in place: {write}");
            assert!(
                take.blocks <= TAKE_BLOCKS,
                "{cols}x{rows} take_frame: {take}, budget {TAKE_BLOCKS}"
            );
            takes.push(take.blocks);
        }
        assert!(
            takes.windows(2).all(|w| w[0] == w[1]),
            "the same blocks on every screen: {takes:?}"
        );
    }

    /// The row read, the copy the viewers' ledger keeps, the frame's list of rows and the
    /// ledger's list.
    const TAKE_BLOCKS: u64 = 4;

    /// An Enter at a bottom prompt ships the rows that came in and nothing else, so its blocks
    /// do not grow with the screen's height.
    #[test]
    fn an_enter_at_the_bottom_allocates_by_the_rows_it_moved() {
        assert!(alloc::installed(), "the counting allocator is this binary's");
        let mut takes = Vec::new();
        for (cols, rows) in SIZES {
            let mut e = at_a_full_prompt(cols, rows);
            e.write(b"ls");
            let _typed = e.take_frame(10).unwrap();
            e.write(b"\r\n\x1b]133;C\x07Cargo.toml  crates  docs\r\napps  vendor  xtask\r\nREADME.md\r\n");
            e.write(prompt());
            let (frame, take) = alloc::measure(|| e.take_frame(11).unwrap());
            let frame = frame.expect("the command's frame");
            eprintln!("{cols}x{rows}: enter take_frame {take}; {} rows", frame.updates.len());
            assert!(!frame.full, "a diff, not every row");
            assert!(take.blocks <= ENTER_BLOCKS, "{cols}x{rows}: {take}, budget {ENTER_BLOCKS}");
            takes.push(take.blocks);
        }
        assert!(
            takes.windows(2).all(|w| w[0] == w[1]),
            "the same blocks on every screen: {takes:?}"
        );
    }

    /// Four rows read and kept, the two lists, and a spare row: until 2026-09-29 every row of
    /// the screen was read into a new line, 30 blocks at 80×24 and 66 at 200×60.
    const ENTER_BLOCKS: u64 = 10;

    /// An echo frame is encoded for the wire in a few blocks, whatever the screen.
    #[test]
    fn encoding_an_echo_frame_is_a_fixed_few_blocks() {
        assert!(alloc::installed(), "the counting allocator is this binary's");
        for (cols, rows) in SIZES {
            let mut e = at_a_full_prompt(cols, rows);
            let (_write, _take, frame) = echo(&mut e, 10);
            let event = TermEvent::Frame(frame);
            let (wire, used) = alloc::measure(|| codec::encode(&event).unwrap());
            eprintln!("{cols}x{rows}: encode {used}; {} B", wire.len());
            assert!(used.blocks <= ENCODE_BLOCKS, "{cols}x{rows}: {used}, budget {ENCODE_BLOCKS}");
            assert_eq!(
                used.bytes,
                wire.len() as u64,
                "{cols}x{rows}: {used} for {} B on the wire",
                wire.len()
            );
        }
    }

    /// The frame itself, copied out of the thread's scratch buffer at its exact size. Until
    /// 2026-09-29 `codec::encode` grew a fresh buffer from the 4-byte prefix as postcard wrote
    /// (4, 8, … 256 bytes for a 230-byte echo): 8 blocks and 532 bytes.
    const ENCODE_BLOCKS: u64 = 1;

    /// One echo frame goes to every viewer as the same encoded bytes: eight viewers cost what
    /// one does.
    #[test]
    fn an_echo_for_many_viewers_is_encoded_once() {
        assert!(alloc::installed(), "the counting allocator is this binary's");
        let fan_out = |viewers: usize| {
            let mut e = at_a_full_prompt(80, 24);
            let mut queues: Vec<Vec<_>> =
                std::iter::repeat_with(|| Vec::with_capacity(4)).take(viewers).collect();
            let ((), used) = alloc::measure(|| {
                e.write(b"l");
                let frame = e.take_frame(10).unwrap().expect("an echo");
                let wire = codec::encode(&TermEvent::Frame(frame)).unwrap();
                for queue in &mut queues {
                    queue.push(wire.clone());
                }
            });
            assert!(queues.iter().all(|q| q.len() == 1), "every viewer was handed the echo");
            used
        };
        let one = fan_out(1);
        let eight = fan_out(8);
        eprintln!("one viewer: {one}; eight viewers: {eight}");
        assert_eq!(eight, one, "eight viewers cost what one does");
        assert!(one.blocks <= FAN_OUT_BLOCKS, "{one}, budget {FAN_OUT_BLOCKS}");
    }

    /// The echo's take (4), its one encoded frame, and the block the first clone shares it
    /// through. Until 2026-09-29 it was 12: the encode's 8 blocks (a buffer grown seven times
    /// from its 4-byte prefix, and the block `Bytes` shared it through) in place of these 2.
    const FAN_OUT_BLOCKS: u64 = 6;
}
