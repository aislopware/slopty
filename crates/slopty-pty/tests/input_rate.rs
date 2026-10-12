//! How fast a program reads what is written into its terminal: the tty's input direction, which
//! a paste and a text dropped on a program (kitty's OSC 72 answers, base64 inside escapes) go
//! through. The app e2e's text fetched on drop moved 12 MiB in 4.2 s, ten times slower than a
//! file over the same link, so the tty's own rate is measured alone here.

#[cfg(test)]
mod input_rate {
    use std::time::{Duration, Instant};

    use slopty_proto::terminal::TermSize;
    use slopty_pty::{Pty, PtyMaster, SpawnSpec};

    /// Bytes written: 8 MiB, of letters and newlines, which no line discipline treats specially.
    const BYTES: usize = 8 << 20;

    /// Read the master until `needle` is in what came, or `limit` passes.
    async fn read_until(master: &PtyMaster, needle: &[u8], limit: Duration) -> Vec<u8> {
        let mut seen = Vec::new();
        let mut buf = [0_u8; 4096];
        let reading = async {
            while !seen.windows(needle.len()).any(|w| w == needle) {
                let n = master.read(&mut buf).await.unwrap();
                assert!(n > 0, "the program ended: {}", String::from_utf8_lossy(&seen));
                seen.extend_from_slice(buf.get(..n).unwrap_or_default());
            }
        };
        tokio::time::timeout(limit, reading).await.expect("the program answered");
        seen
    }

    /// 8 MiB written into a terminal whose program reads it raw, as fast as the tty takes it.
    /// Run with `cargo nextest run -p slopty-pty --release --test input_rate --run-ignored only
    /// --no-capture`.
    #[tokio::test]
    #[ignore = "measurement, run by hand"]
    async fn a_raw_program_reads_what_is_written_at_the_ttys_rate() {
        for (mode, stty) in [("raw", "stty raw -echo"), ("-icanon", "stty -echo -icanon")] {
            let pty = Pty::open(TermSize::default()).unwrap();
            let script = format!("{stty}; echo ready; head -c {BYTES} > /dev/null; echo done");
            let spec = SpawnSpec {
                command: vec!["/bin/sh".into(), "-c".into(), script],
                cwd: None,
                env: Vec::new(),
                size: TermSize::default(),
            };
            let spawned = pty.spawn(&spec).unwrap();
            let master = PtyMaster::new(pty.into_master()).unwrap();
            read_until(&master, b"ready", Duration::from_secs(10)).await;
            let data: Vec<u8> =
                (0..BYTES).map(|i| b"abcdefghijklmnopqrstuvwxyz\n"[i % 27]).collect();

            let started = Instant::now();
            let (written, _done) = tokio::join!(
                async {
                    master.write_all(&data).await.unwrap();
                    started.elapsed()
                },
                read_until(&master, b"done", Duration::from_secs(120)),
            );
            let took = started.elapsed();
            #[expect(clippy::cast_precision_loss, reason = "a rate for people to read")]
            let rate = BYTES as f64 / took.as_secs_f64() / 1e6;
            println!(
                "MEASURE tty input, {mode}: {BYTES} bytes read by the program in {:.0} ms \
                 ({rate:.1} MB/s), all written after {:.0} ms",
                took.as_secs_f64() * 1e3,
                written.as_secs_f64() * 1e3,
            );
            let mut child = spawned.child;
            let _ended = child.wait().await;
        }
    }
}
