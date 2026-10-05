//! The chrome's SF Symbols, drawn by the running OS (`slopty_platform::symbols`).

#![cfg(any(target_os = "macos", target_os = "ios"))]

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use slopty_platform::symbols::{
        Masks, REMEMBERED, Scale, Symbol, SymbolMask, SymbolSize, Weight, exists, rasterize,
        remembered,
    };

    /// The chrome's body size: a navigator row's text.
    const BODY: SymbolSize = SymbolSize::new(13.0, Weight::Regular);

    fn draw(symbol: Symbol, size: SymbolSize, scale: f32) -> SymbolMask {
        rasterize(symbol, size, scale)
            .unwrap_or_else(|| panic!("{} draws at {size:?} @{scale}", symbol.name()))
    }

    /// The box of pixels with any ink, as `(left, top, right, bottom)`, exclusive at the end.
    #[expect(clippy::arithmetic_side_effects, reason = "a test, which fails by panicking")]
    fn ink(mask: &SymbolMask) -> (u32, u32, u32, u32) {
        let mut found = (u32::MAX, u32::MAX, 0, 0);
        for (i, &alpha) in mask.alpha.iter().enumerate() {
            if alpha > 0 {
                let i = u32::try_from(i).expect("a mask is small");
                let (x, y) = (i % mask.width, i / mask.width);
                found = (found.0.min(x), found.1.min(y), found.2.max(x + 1), found.3.max(y + 1));
            }
        }
        found
    }

    fn coverage(mask: &SymbolMask) -> u64 {
        mask.alpha.iter().map(|&alpha| u64::from(alpha)).sum()
    }

    #[test]
    fn every_symbol_the_chrome_draws_is_on_this_os() {
        let missing: Vec<_> = Symbol::ALL
            .iter()
            .filter(|symbol| !exists(**symbol))
            .map(|symbol| symbol.name())
            .collect();
        assert!(missing.is_empty(), "missing on this OS: {missing:?}");
        for &symbol in Symbol::ALL {
            let mask = draw(symbol, BODY, 1.0);
            assert!(coverage(&mask) > 0, "{} draws nothing", symbol.name());
        }
    }

    #[test]
    fn a_mask_is_drawn_at_whole_device_pixels_with_its_alignment_inside() {
        let one = draw(Symbol::Checkmark, BODY, 1.0);
        let two = draw(Symbol::Checkmark, BODY, 2.0);
        for mask in [&one, &two] {
            assert_eq!(mask.alpha.len(), (mask.width * mask.height) as usize);
            let a = mask.alignment;
            assert!(a.width > 0.0 && a.height > 0.0, "{a:?}");
            #[expect(clippy::cast_precision_loss, reason = "a mask is small")]
            let (width, height) = (mask.width as f32, mask.height as f32);
            assert!(a.x >= 0.0 && a.y >= 0.0, "{a:?}");
            assert!(
                a.x + a.width <= width + 0.01 && a.y + a.height <= height + 0.01,
                "{a:?} in {width}x{height}"
            );
            assert!(
                mask.baseline > a.y && mask.baseline <= height + 0.01,
                "baseline {} in {a:?}",
                mask.baseline
            );
            assert!(mask.alpha.iter().any(|&alpha| alpha >= 230), "a solid pixel somewhere");
            assert!(mask.alpha.contains(&0), "a clear pixel somewhere");
        }
        // Twice the device pixels, give or take the rounding up of each.
        assert!(two.width.abs_diff(one.width * 2) <= 1, "{} against {}", two.width, one.width);
        assert!(two.height.abs_diff(one.height * 2) <= 1, "{} against {}", two.height, one.height);
        assert!(one.alignment.width.mul_add(-2.0, two.alignment.width).abs() < 0.01);
        assert!(one.baseline.mul_add(-2.0, two.baseline).abs() < 0.01);
    }

    /// The alignment rectangle runs from the baseline to the cap height, so a symbol whose ink is
    /// even top and bottom centres on its middle line, as it does on the text's beside it.
    #[test]
    fn even_ink_centres_on_the_alignment_rectangle_from_the_baseline() {
        let even = [
            Symbol::Circle,
            Symbol::Plus,
            Symbol::Xmark,
            Symbol::Minus,
            Symbol::Ellipsis,
            Symbol::StopFill,
            Symbol::Gearshape,
        ];
        for symbol in even {
            for scale in [1.0, 2.0] {
                let mask = draw(symbol, BODY, scale);
                let (_, top, _, bottom) = ink(&mask);
                let a = mask.alignment;
                #[expect(clippy::cast_precision_loss, reason = "a mask is small")]
                let middle = (top + bottom) as f32 / 2.0;
                assert!(
                    (middle - (a.y + a.height / 2.0)).abs() <= scale,
                    "{} @{scale}: ink {top}..{bottom} against {a:?}",
                    symbol.name()
                );
                assert!(
                    (mask.baseline - (a.y + a.height)).abs() <= 0.5 * scale,
                    "{a:?} {}",
                    mask.baseline
                );
            }
        }
    }

    #[test]
    fn a_heavier_weight_and_a_larger_scale_draw_more_ink() {
        let light = draw(Symbol::Folder, SymbolSize::new(13.0, Weight::Light), 2.0);
        let semibold = draw(Symbol::Folder, SymbolSize::new(13.0, Weight::Semibold), 2.0);
        assert!(coverage(&semibold) > coverage(&light));
        let small = draw(Symbol::ChevronDown, BODY.scaled(Scale::Small), 2.0);
        let large = draw(Symbol::ChevronDown, BODY.scaled(Scale::Large), 2.0);
        assert!(large.alignment.width > small.alignment.width);
    }

    #[test]
    fn a_size_that_draws_nothing_is_none() {
        assert!(rasterize(Symbol::Plus, SymbolSize::new(0.0, Weight::Regular), 2.0).is_none());
        assert!(rasterize(Symbol::Plus, SymbolSize::new(f32::NAN, Weight::Regular), 2.0).is_none());
        assert!(rasterize(Symbol::Plus, BODY, 0.0).is_none());
        let masks = Masks::new();
        assert!(masks.get(Symbol::Plus, BODY, 0.0).is_none());
        assert_eq!(masks.len(), 1, "the miss is kept");
    }

    /// Off the main thread, from many threads at once, a symbol draws the same bytes as alone.
    #[test]
    fn symbols_draw_on_any_thread_and_at_once() {
        let alone: Vec<_> = Symbol::ALL.iter().map(|&symbol| draw(symbol, BODY, 2.0)).collect();
        let alone = Arc::new(alone);
        let threads: Vec<_> = (0..8)
            .map(|offset| {
                let alone = Arc::clone(&alone);
                std::thread::spawn(move || {
                    for (i, &symbol) in Symbol::ALL
                        .iter()
                        .enumerate()
                        .cycle()
                        .skip(offset * 11)
                        .take(Symbol::ALL.len())
                    {
                        assert_eq!(draw(symbol, BODY, 2.0), alone[i], "{}", symbol.name());
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("a drawing thread finishes");
        }
    }

    #[test]
    fn a_mask_is_drawn_once_and_shared() {
        let masks = Masks::new();
        let first = masks.get(Symbol::Gearshape, BODY, 2.0).expect("drawn");
        let again = masks.get(Symbol::Gearshape, BODY, 2.0).expect("kept");
        assert!(Arc::ptr_eq(&first, &again));
        assert!(
            masks.kept(Symbol::Gearshape, BODY, 1.0).is_none(),
            "another scale is another mask"
        );
        assert_eq!(masks.len(), 1);
    }

    #[test]
    fn a_prewarm_draws_every_mask_asked_for() {
        let masks = Masks::new();
        let wanted: Vec<_> = Symbol::ALL.iter().map(|&symbol| (symbol, BODY, 2.0)).collect();
        masks.prewarm(move || wanted).expect("the thread starts").join().expect("it finishes");
        assert_eq!(masks.len(), Symbol::ALL.len());
        for &symbol in Symbol::ALL {
            assert!(masks.kept(symbol, BODY, 2.0).is_some(), "{}", symbol.name());
        }
    }

    /// What painters asked for is written down, by display scale, and read back as the next
    /// prewarm. A prewarm's own drawings are not in it.
    #[test]
    fn what_painters_asked_for_is_the_next_prewarm() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("symbols");
        let small = SymbolSize::new(11.0, Weight::Semibold).scaled(Scale::Small);
        let masks = Masks::new();
        let warmed = vec![(Symbol::Trash, BODY, 2.0)];
        masks.prewarm(move || warmed).expect("starts").join().expect("finishes");
        drop(masks.get(Symbol::Checkmark, BODY, 2.0));
        drop(masks.get(Symbol::ChevronRight, small, 2.0));
        drop(masks.get(Symbol::Checkmark, BODY, 2.0));
        masks.remember(&path).expect("written");
        // Asked for after: not noted.
        drop(masks.get(Symbol::Xmark, BODY, 2.0));
        let at_two = vec![(Symbol::Checkmark, BODY, 2.0), (Symbol::ChevronRight, small, 2.0)];
        assert_eq!(remembered(&path, Some(2.0)), at_two);
        assert_eq!(remembered(&path, None), at_two);
        // A display of another scale warms the same symbols at its own.
        let at_one = vec![(Symbol::Checkmark, BODY, 1.0), (Symbol::ChevronRight, small, 1.0)];
        assert_eq!(remembered(&path, Some(1.0)), at_one);

        // A launch on the other display keeps the first display's lines.
        let other = Masks::new();
        drop(other.get(Symbol::Plus, BODY, 1.0));
        other.remember(&path).expect("written");
        assert_eq!(remembered(&path, Some(1.0)), vec![(Symbol::Plus, BODY, 1.0)]);
        assert_eq!(remembered(&path, Some(2.0)), at_two);
        assert!(!dir.path().join("symbols.new").exists(), "renamed into place");
    }

    /// A missing file, or lines that do not read, warm nothing rather than fail; a list is
    /// read no further than its bound.
    #[test]
    fn a_missing_or_damaged_list_warms_nothing() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("symbols");
        assert_eq!(remembered(&path, Some(2.0)), Vec::new());
        let damaged = "checkmark 13 regular medium 2\nno.such.symbol 13 regular medium 2\n\
                       checkmark NaN regular medium 2\ncheckmark 13 bold medium 2\n\
                       checkmark 13 regular medium\nchevron.right 13 regular medium 2 extra\n\
                       xmark 13 regu";
        std::fs::write(&path, damaged).expect("written");
        assert_eq!(remembered(&path, Some(2.0)), vec![(Symbol::Checkmark, BODY, 2.0)]);
        std::fs::write(&path, "checkmark 13 regular medium 2\n".repeat(REMEMBERED + 50))
            .expect("written");
        assert_eq!(remembered(&path, None).len(), REMEMBERED);
    }

    fn median(mut times: Vec<Duration>) -> Duration {
        times.sort_unstable();
        times[times.len() / 2]
    }

    /// What a raster costs: the first of each symbol in this process, and again; and the whole
    /// list prewarmed at the chrome's body size. Prints; `docs/MEASUREMENTS.md`, "SF Symbols as
    /// masks".
    #[test]
    #[ignore = "a measurement"]
    fn measure_symbol_rasters() {
        // The first pass is the process's first look at each symbol; the second is the same
        // symbols at the other scale, and the third the same mask drawn again.
        let passes = [("first in the process", 2.0), ("at another scale", 1.0), ("again", 2.0)];
        for (pass, scale) in passes {
            let mut times = Vec::new();
            for &symbol in Symbol::ALL {
                let start = Instant::now();
                drop(draw(symbol, BODY, scale));
                times.push(start.elapsed());
            }
            let total: Duration = times.iter().sum();
            println!(
                "{pass} @{scale}x: median {:?}, max {:?}, {total:?} for {}",
                median(times.clone()),
                times.iter().max().copied().unwrap_or_default(),
                Symbol::ALL.len(),
            );
        }
        for scale in [1.0, 2.0] {
            let masks = Masks::new();
            let sizes = [
                SymbolSize::new(12.0, Weight::Medium),
                SymbolSize::new(11.0, Weight::Semibold).scaled(Scale::Small),
                SymbolSize::new(12.0, Weight::Semibold),
            ];
            let wanted: Vec<_> = sizes
                .iter()
                .flat_map(|&size| Symbol::ALL.iter().map(move |&symbol| (symbol, size, scale)))
                .collect();
            let count = wanted.len();
            let start = Instant::now();
            masks.prewarm(move || wanted).expect("starts").join().expect("finishes");
            println!("prewarm @{scale}x: {count} masks in {:?}", start.elapsed());
            let start = Instant::now();
            for &size in &sizes {
                for &symbol in Symbol::ALL {
                    drop(masks.get(symbol, size, scale));
                }
            }
            println!("kept @{scale}x: {count} lookups in {:?}", start.elapsed());
        }
    }

    /// Drawing one mask again and again on a thread of its own, as the prewarm does, leaves the
    /// footprint where the first drawing put it: nothing a raster makes outlives it. Without a
    /// pool around each raster, each one's autoreleased image and context stayed until the
    /// thread ended, about 58 KB a drawing (`docs/MEASUREMENTS.md`, "the footprint at rest").
    #[cfg(target_os = "macos")]
    #[test]
    fn a_raster_leaves_nothing_behind_on_its_thread() {
        const DRAWS: u64 = 2000;
        let footprint = || slopty_testkit::process::own().expect("this process's usage").footprint;
        let grown = std::thread::spawn(move || {
            drop(draw(Symbol::Checkmark, BODY, 2.0));
            let first = footprint();
            for _ in 0..DRAWS {
                drop(draw(Symbol::Checkmark, BODY, 2.0));
            }
            footprint().saturating_sub(first)
        })
        .join()
        .expect("the thread draws");
        // 0.6 MB measured with the pool, 116 MB without; the rest is other tests drawing at
        // the same time in this process.
        assert!(grown < 20_000_000, "{} KB kept by {DRAWS} drawings", grown / 1000);
    }

    /// What a prewarm of every symbol at the chrome's four sizes on two scales, as the launch
    /// prewarm drew before it read what the last launch drew, leaves in the footprint. Prints;
    /// run alone, it reads this process's footprint.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "measurement: run alone, it reads this process's footprint"]
    fn measure_prewarm_footprint() {
        let usage = || slopty_testkit::process::own().expect("this process's usage");
        let before = usage();
        let masks = Masks::new();
        let sizes = [
            SymbolSize::new(15.0, Weight::Regular),
            SymbolSize::new(13.0, Weight::Regular),
            SymbolSize::new(11.0, Weight::Semibold),
            SymbolSize::new(10.0, Weight::Medium),
        ];
        let threads: Vec<_> = [1.0, 2.0]
            .into_iter()
            .map(|scale| {
                let wanted: Vec<_> = sizes
                    .iter()
                    .flat_map(|&size| Symbol::ALL.iter().map(move |&s| (s, size, scale)))
                    .collect();
                masks.prewarm(move || wanted).expect("starts")
            })
            .collect();
        for thread in threads {
            thread.join().expect("finishes");
        }
        let after = usage();
        let mb = |bytes: u64| bytes / 1_000_000;
        println!(
            "MEASURE prewarm: {} masks; footprint {} MB before, {} MB after, peak {} MB",
            masks.len(),
            mb(before.footprint),
            mb(after.footprint),
            mb(after.peak_footprint)
        );
    }
}
