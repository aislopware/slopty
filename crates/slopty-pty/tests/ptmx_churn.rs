//! Pseudo-terminals opened beside others being closed.

#[cfg(test)]
mod churn {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use rustix::process::Signal;
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

    /// Six processes open and close pairs for two minutes, so a minor is handed out again while
    /// the pair that last had it, in another process, is still taking its node down: XNU then
    /// makes the new pair no slave, and `grantpt` on it spins in the kernel for good unless
    /// `Pty::open` sees that first. No process may wedge. The race needs the processes: eight
    /// threads of one never met it in twelve runs; six processes met it about once a minute.
    #[test]
    #[ignore = "a probe of the kernel, run by hand: cargo nextest run -p slopty-pty --test ptmx_churn --run-ignored only --no-capture opens_beside_closes_in_other_processes"]
    fn opens_beside_closes_in_other_processes_never_wedge() {
        let me = std::env::current_exe().unwrap();
        let (exited, exits) = std::sync::mpsc::channel();
        let pids: Vec<_> = std::iter::repeat_with(|| {
            let mut churner = std::process::Command::new(&me)
                .args(["--exact", "churn::one_process_of_the_churn", "--ignored"])
                .env(CHURNER, "1")
                .spawn()
                .unwrap();
            let pid = rustix::process::Pid::from_child(&churner);
            let exited = exited.clone();
            std::thread::spawn(move || exited.send(churner.wait().unwrap()).unwrap());
            pid
        })
        .take(6)
        .collect();
        let deadline = Instant::now() + CHURN + Duration::from_secs(60);
        for _ in &pids {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(status) = exits.recv_timeout(left) else {
                for pid in pids {
                    let _killed = rustix::process::kill_process(pid, Signal::KILL);
                }
                panic!("a churner wedged: an open is stuck in the kernel");
            };
            assert!(status.success(), "a churner failed: {status}");
        }
    }

    /// Set in the processes [`opens_beside_closes_in_other_processes_never_wedge`] starts.
    const CHURNER: &str = "SLOPTY_PTMX_CHURNER";

    /// How long each of those processes churns.
    const CHURN: Duration = Duration::from_secs(120);

    /// One process of [`opens_beside_closes_in_other_processes_never_wedge`]; nothing when run
    /// any other way.
    #[test]
    #[ignore = "started by opens_beside_closes_in_other_processes_never_wedge"]
    fn one_process_of_the_churn() {
        if std::env::var_os(CHURNER).is_none() {
            return;
        }
        let end = Instant::now() + CHURN;
        while Instant::now() < end {
            drop(Pty::open(TermSize::default()).unwrap());
        }
    }
}
