//! Screen pipeline on real hardware: list, open the first display, count datagrams, close.
//! Needs Screen Recording permission, so it runs only with `SLOPTY_SCREEN_E2E=1`.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_host::screen::{DATAGRAM_QUEUE, DatagramBudget, ScreenStream, listing};
    use slopty_proto::media::{Kind, MediaHeader, flags};
    use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent};
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn display_stream_produces_datagrams() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let ScreenEvent::Listing { windows, displays } = listing().await.unwrap() else {
            panic!("listing")
        };
        eprintln!("{} windows, {} displays", windows.len(), displays.len());
        let display = displays.first().expect("a display");

        let (tx, mut rx) = mpsc::channel(DATAGRAM_QUEUE);
        let quality = Quality { fps: 60, bitrate_bps: 8_000_000, scale: 0.5, ..Quality::default() };
        let (mut stream, opened) = ScreenStream::open(
            slopty_core::StreamId(7),
            CaptureTarget::Display(display.id),
            quality,
            tx,
            DatagramBudget::new(),
            |e| panic!("capture stopped: {e}"),
        )
        .await
        .unwrap();
        eprintln!("{opened:?}");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let (mut data, mut parity, mut cursor, mut keyframes) = (0_u32, 0_u32, 0_u32, 0_u32);
        let mut frames = std::collections::BTreeSet::new();
        while let Ok(Some(datagram)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            let (header, _payload) = MediaHeader::parse(&datagram).expect("well-formed");
            match Kind::from_u8(header.kind) {
                Some(Kind::VideoData) => {
                    data += 1;
                    frames.insert(header.frame.get());
                    if header.index.get() == 0 && header.flags & flags::KEYFRAME != 0 {
                        keyframes += 1;
                    }
                }
                Some(Kind::VideoParity) => parity += 1,
                Some(Kind::Cursor) => cursor += 1,
                _other => {}
            }
        }
        let stats = stream.stats();
        eprintln!(
            "frames {} data {data} parity {parity} cursor {cursor} keyframes {keyframes} {stats:?}",
            frames.len()
        );
        assert!(keyframes >= 1, "first frame is an IDR");
        assert!(frames.len() >= 10, "steady stream, got {} frames", frames.len());
        assert_eq!(stats.encoded, frames.len() as u64);

        // Bitrate-only change applies in place; a scale change rebuilds capture and encoder.
        stream.set_quality(&Quality { bitrate_bps: 4_000_000, ..quality }).unwrap();
        stream.set_quality(&Quality { scale: 0.25, ..quality }).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        let mut after = 0_u32;
        while let Ok(Some(_datagram)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            after += 1;
        }
        assert!(after > 0, "stream keeps flowing after a quality change");
        stream.close().await;
    }
}
