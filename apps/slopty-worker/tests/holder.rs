//! The display holder as the worker's binary runs it (`slopty-worker --display-holder`), making
//! real displays: one made for a worker outlives that worker, and the next worker finds it.

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use std::path::Path;

    use slopty_proto::screen::{DisplayKey, DisplayShape};
    use slopty_worker::screen::holder::{self, Need, Remote};
    use slopty_worker::screen::sized::{self, Factory as _};

    /// A worker's link to the holder at `socket`, starting it from this build's worker when
    /// `start` and none answers.
    fn worker(socket: &Path, store: &Path, start: bool) -> Remote {
        let program =
            slopty_testkit::bins::bin(env!("CARGO_BIN_EXE_slopty-worker"), "slopty-worker");
        let (socket, store) = (socket.to_path_buf(), store.to_path_buf());
        Remote::new(Box::new(move |need| match need {
            Need::Make | Need::Fresh if start => holder::start(&program, &socket, &store),
            _ => std::os::unix::net::UnixStream::connect(&socket),
        }))
    }

    /// A display the holder made for a worker that then went without letting go is still
    /// there for the next worker, under the same id; ended, it leaves the holder's list.
    #[test]
    fn a_display_the_holder_made_outlives_the_worker_that_asked_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, store) = (dir.path().join("displays.sock"), dir.path().join("displays.json"));
        let key = DisplayKey(*b"slopty-live-hold");
        let shape = DisplayShape { width: 1280, height: 800, scale: 2.0, refresh_hz: 60 };
        let plan = sized::plan_for(key, &shape);

        let mut first = worker(&socket, &store, true);
        let made = first.create(key, &plan).expect("the holder made a display");
        let id = first.id(&made);
        assert_ne!(id, 0);
        assert_eq!(holder::kept(&store), [(key, plan)], "listed on disk");
        drop(first);

        let mut second = worker(&socket, &store, false);
        let held = second.held();
        let [(held_key, held_plan, display)] = held.as_slice() else { panic!("one display") };
        assert_eq!((*held_key, *held_plan, second.id(display)), (key, plan, id), "the same one");
        second.release(*display);
        assert!(holder::kept(&store).is_empty(), "ended, and out of the list");
        assert_eq!(second.held().len(), 0, "nothing held");
        second.quit();
    }
}
