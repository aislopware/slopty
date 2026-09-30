//! What the hub keeps on the heap however long it runs: its bounded event log, and nothing
//! more. A soak cycle as the hub sees it (a terminal opens, its agent works and goes idle, it
//! closes) logs four events, so the log fills at `EVENT_LOG / 4` cycles.
//!
//! The binary counts the bytes each thread holds (allocated less freed), and the hub is driven
//! on the test's own thread with no runtime, so every block it keeps is counted here.

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::net::{IpAddr, Ipv6Addr, SocketAddr};

    use slopty_core::{SessionId, WallMs, WorkerId};
    use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, AgentStatus};
    use slopty_proto::orchestration::HubEvent;
    use slopty_proto::server::{Os, Registration, ToServer, WorkerCaps};
    use slopty_proto::terminal::{CloseReason, SessionState, SessionSummary};
    use slopty_server::Hub;
    use slopty_server::hub::EVENT_LOG;

    thread_local! {
        static HELD: Cell<isize> = const { Cell::new(0) };
    }

    fn hold(bytes: usize, sign: isize) {
        let bytes = isize::try_from(bytes).unwrap_or(isize::MAX);
        // `try_with`: a thread being torn down still frees, and an allocator must not panic.
        let _held = HELD.try_with(|h| h.set(h.get().wrapping_add(sign.wrapping_mul(bytes))));
    }

    fn held() -> isize {
        HELD.with(Cell::get)
    }

    /// [`System`], keeping each thread's allocated-less-freed bytes.
    struct Held;

    // SAFETY: every method forwards to `System` with the caller's arguments unchanged, so the
    // `GlobalAlloc` contract `System` meets is met here; the bookkeeping touches only a
    // const-initialised thread-local cell with no destructor, which never allocates.
    unsafe impl GlobalAlloc for Held {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            hold(layout.size(), 1);
            // SAFETY: the caller's `layout` is forwarded as `GlobalAlloc::alloc` requires.
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            hold(layout.size(), 1);
            // SAFETY: as `alloc`.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            hold(layout.size(), -1);
            // SAFETY: `ptr` came from this allocator, which is `System`, with this `layout`.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            hold(layout.size(), -1);
            hold(new_size, 1);
            // SAFETY: `ptr` came from `System` with `layout`, and the caller guarantees
            // `new_size` is valid for it, as `GlobalAlloc::realloc` requires.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static ALLOC: Held = Held;

    fn caps() -> WorkerCaps {
        WorkerCaps {
            os: Os::MacOs,
            os_version: "26.5".to_owned(),
            arch: "aarch64".to_owned(),
            cpus: 10,
            memory: 32 << 30,
            encoders: Vec::new(),
            displays: Vec::new(),
            agents: Vec::new(),
            can_capture: false,
            can_inject: false,
            virtual_displays: false,
            version: "0".to_owned(),
            lan: Vec::new(),
            wake_on_lan: None,
        }
    }

    /// A quiet bash as the soak opens it.
    fn bash(id: SessionId) -> SessionSummary {
        SessionSummary {
            id,
            title: "bash".to_owned(),
            cwd: Some("/var/folders/xy/T/slopty-soak-1".to_owned()),
            repo: None,
            branch: None,
            changes: None,
            started_ms: WallMs::now(),
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 0,
            command: ["/bin/bash", "--noprofile", "--norc", "-i"].map(str::to_owned).to_vec(),
            agent: None,
            progress: None,
            restored: None,
        }
    }

    fn agent(session: SessionId, status: AgentStatus) -> ToServer {
        ToServer::Agent(AgentEvent {
            session,
            kind: AgentKind::ClaudeCode,
            status,
            agent_session: None,
            detail: None,
            attention: false,
            source: AgentSource::Hook,
            since_ms: WallMs::now(),
        })
    }

    /// However long terminals open, their agents work and they close, the hub holds no more
    /// than its event log's bound of them, and once the log is full its heap stays level. The
    /// log used to push before it dropped the oldest, so the one event over the bound doubled
    /// its buffer, room for twice the events it keeps, which the ring then wrote through: the
    /// soak's server growth.
    #[test]
    fn the_hub_holds_its_event_log_and_no_more() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let registration = Registration {
            worker,
            name: "soak".to_owned(),
            listen: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 45550)),
            caps: caps(),
            sessions: Vec::new(),
            session_key: [7; 32],
        };
        let lease = hub.register(registration, IpAddr::from([127, 0, 0, 1]), tx).unwrap();
        let empty = held();
        let cycles = |n: usize| {
            for _ in 0..n {
                let session = SessionId::new();
                lease.handle(ToServer::SessionChanged(bash(session)));
                lease.handle(agent(session, AgentStatus::Working));
                lease.handle(agent(session, AgentStatus::Idle));
                lease.handle(ToServer::SessionClosed { session, reason: CloseReason::Requested });
            }
        };
        let fill = EVENT_LOG / 4;
        cycles(2 * fill);
        let full = held();
        cycles(2 * fill);
        let later = held();

        let summary = {
            let before = held();
            let one = bash(SessionId::new());
            let heap = held() - before;
            drop(one);
            usize::try_from(heap).unwrap()
        };
        // Every slot of the log, the heap of the terminals it holds (a quarter of its events
        // open one), and a little for the registry's own tables.
        let bound = EVENT_LOG * size_of::<HubEvent>() + fill * summary + (16 << 10);
        let hub_heap = usize::try_from(later - empty).unwrap();
        println!(
            "the hub grew {} KiB over {} cycles, {:+} B over the {} after, bound {} KiB",
            (full - empty) / 1024,
            2 * fill,
            later - full,
            2 * fill,
            bound / 1024
        );
        assert!(
            hub_heap <= bound,
            "the hub grew by {hub_heap} B, more than the log holds ({bound} B)"
        );
        assert_eq!(later, full, "a full log grew by {} B", later - full);
    }
}
