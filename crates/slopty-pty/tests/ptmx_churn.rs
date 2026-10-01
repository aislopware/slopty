//! Pseudo-terminals opened beside others being closed, far below `kern.tty.ptmx_max`.

#[cfg(test)]
mod churn {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use slopty_proto::terminal::TermSize;
    use slopty_pty::Pty;

    /// One thread opens 400 pairs and holds them all, while four others open and close one
    /// each as fast as they can: at most 404 of ours open at once, against the 511 allowed.
    /// The kernel's table of pairs grows 16 at a time as the held ones pass each multiple of
    /// 16. Every refusal is counted with how many were held then.
    #[test]
    #[ignore = "a probe of the kernel, run by hand: cargo nextest run -p slopty-pty --test ptmx_churn --run-ignored only --no-capture"]
    fn opens_beside_closes_are_not_refused_below_the_limit() {
        let stop = Arc::new(AtomicBool::new(false));
        let churned = Arc::new(AtomicUsize::new(0));
        let churners: Vec<_> = std::iter::repeat_with(|| {
            let (stop, churned) = (Arc::clone(&stop), Arc::clone(&churned));
            std::thread::spawn(move || {
                let mut refused = 0_usize;
                while !stop.load(Ordering::Relaxed) {
                    match Pty::open(TermSize::default()) {
                        Ok(pty) => drop(pty),
                        Err(_) => refused += 1,
                    }
                    churned.fetch_add(1, Ordering::Relaxed);
                }
                refused
            })
        })
        .take(4)
        .collect();
        let mut held = Vec::new();
        let mut refused = Vec::new();
        while held.len() < 400 {
            match Pty::open(TermSize::default()) {
                Ok(pty) => held.push(pty),
                Err(e) => {
                    refused.push(held.len());
                    if refused.len() <= 5 {
                        eprintln!("refused with {} held: {e}", held.len());
                    }
                    if refused.len() > 1000 {
                        break;
                    }
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        let mut churn_refused = 0_usize;
        for churner in churners {
            churn_refused += churner.join().unwrap();
        }
        eprintln!(
            "held {}, the holder refused {} times (at {:?}), the churners {churn_refused} of {}",
            held.len(),
            refused.len(),
            &refused[..refused.len().min(20)],
            churned.load(Ordering::Relaxed),
        );
        assert!(refused.is_empty() && churn_refused == 0, "opens refused below the limit");
    }
}
