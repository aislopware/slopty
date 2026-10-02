//! A worker's domain against a real worker daemon on this machine: what the system is given
//! when it asks for an item, a folder or a file's bytes, what it hears when a watched folder
//! changes on the worker, and what it is told when the worker cannot answer.

#![cfg(target_os = "macos")]

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Arc;
    use std::time::Duration;

    use slopty_core::{ClientId, WorkerId, XferId};
    use slopty_files::changes::Change;
    use slopty_files::domain::{Domain, Signal};
    use slopty_files::item::ROOT;
    use slopty_files::worker::FilesError;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_platform::files::{Directory, Known};
    use slopty_proto::handshake::Hello;
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};
    use tokio::sync::Notify;

    const STEP: Duration = Duration::from_secs(30);

    /// A binary of this build (`slopty_testkit::bins`).
    fn bin(name: &str) -> PathBuf {
        slopty_testkit::bins::bin(env!("CARGO_BIN_EXE_slopty-files"), name)
    }

    /// ptyd and a worker whose home is `dir/home`, killed with the test.
    struct Daemons {
        _ptyd: Child,
        worker: Child,
        addr: SocketAddr,
        id: WorkerId,
        home: PathBuf,
    }

    impl Daemons {
        /// Stop the worker and start it again on the same port and data directory, as an
        /// update restarts it: the same worker at the same address.
        async fn restart_worker(&mut self, dir: &Path) {
            self.worker.kill().await.unwrap();
            let (worker, addr) = worker(dir, &self.home, self.addr.port()).await;
            assert_eq!(addr, self.addr, "back at its address");
            self.worker = worker;
        }
    }

    fn scrubbed(program: PathBuf, home: &Path) -> Command {
        let mut command = Command::new(program);
        slopty_testkit::env::scrub(command.as_std_mut(), home);
        command.kill_on_drop(true);
        command
    }

    async fn daemons(dir: &Path) -> Daemons {
        let home = dir.join("home");
        std::fs::create_dir_all(home.join("src")).unwrap();
        std::fs::write(home.join("a.txt"), b"hello").unwrap();
        std::fs::write(home.join("src/main.rs"), b"fn main() {}\n").unwrap();
        let ptyd_sock = dir.join("ptyd.sock");
        let ptyd = scrubbed(bin("slopty-ptyd"), &home)
            .arg("--socket")
            .arg(&ptyd_sock)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let ready = tokio::time::timeout(STEP, async {
            while tokio::net::UnixStream::connect(&ptyd_sock).await.is_err() {
                // The socket appears once ptyd listens; polled, as nothing announces it.
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        });
        ready.await.expect("ptyd listens");
        let (worker, addr) = worker(dir, &home, 0).await;
        let endpoint = bind_client().unwrap();
        let hello = Hello { client: ClientId::new(), name: "test".to_owned() };
        let conn = tokio::time::timeout(STEP, connect_addr(&endpoint, addr, hello));
        let id = conn.await.unwrap().unwrap().ack.worker;
        Daemons { _ptyd: ptyd, worker, addr, id, home }
    }

    /// The worker on `dir`'s ptyd and data directory, listening on `port` (any when 0), and its
    /// address on loopback.
    async fn worker(dir: &Path, home: &Path, port: u16) -> (Child, SocketAddr) {
        let mut worker = scrubbed(bin("slopty-worker"), home)
            .arg("--ptyd-socket")
            .arg(dir.join("ptyd.sock"))
            .arg("--ctl-socket")
            .arg(dir.join("worker.sock"))
            .arg("--data-dir")
            .arg(dir.join("data"))
            .arg("--print-addr")
            .arg("--port")
            .arg(port.to_string())
            .env(
                "SLOPTY_PASTEBOARD",
                format!("dev.aislopware.slopty.files-test.{}", ClientId::new()),
            )
            .env("SLOPTY_DROP_DIR", dir.join("drop"))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut line = String::new();
        let stdout = worker.stdout.take().unwrap();
        let mut stdout = BufReader::new(stdout);
        let read = tokio::time::timeout(STEP, stdout.read_line(&mut line));
        read.await.expect("the worker prints its address").unwrap();
        let bound: SocketAddr = line.trim().parse().unwrap();
        (worker, SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, bound.port())))
    }

    /// A domain of the worker `id`, reached at `addr` by the directory written in `shared`,
    /// and a notice of each signal it gives.
    fn domain(shared: &Path, id: WorkerId, addr: &str) -> (Domain, Arc<Notify>) {
        let known = Known { id, name: "studio".to_owned(), addrs: vec![addr.to_owned()] };
        Directory { workers: vec![known] }.write(shared).unwrap();
        let heard = Arc::new(Notify::new());
        let notify = Arc::clone(&heard);
        let signal: Signal = Arc::new(move || notify.notify_one());
        (Domain::new(id, shared.to_path_buf(), signal), heard)
    }

    /// The root is the worker's home under the worker's name, a folder lists its files and
    /// folders with their sizes, an item deeper down is found by its path, and a file's bytes
    /// come down whole with the item they are of.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_workers_home_lists_and_its_files_come_down() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let (domain, _heard) =
            domain(&dir.path().join("shared"), daemons.id, &daemons.addr.to_string());

        let root = domain.item(ROOT).await.unwrap();
        assert_eq!((root.name.as_str(), root.folder), ("studio", true));
        let mut listed = domain.list(ROOT).await.unwrap();
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        // The worker keeps dot files of its own in its home (its terminfo); the test made the rest.
        let seen: Vec<_> = listed
            .iter()
            .filter(|i| !i.name.starts_with('.'))
            .map(|i| (i.id.as_str(), i.folder, i.size))
            .collect();
        assert_eq!(seen, [("a.txt", false, 5), ("src", true, 0)]);
        let main = domain.item("src/main.rs").await.unwrap();
        assert_eq!((main.parent.as_str(), main.size), ("src", 13));

        let into = dir.path().join("fetched");
        std::fs::create_dir_all(&into).unwrap();
        let (landed, item) = domain.fetch("a.txt", &into, XferId::new()).await.unwrap();
        assert_eq!(std::fs::read(&landed).unwrap(), b"hello");
        assert!(landed.starts_with(&into), "{landed:?}");
        let a = listed.iter().find(|i| i.id == "a.txt").unwrap();
        assert_eq!(item.content_version(), a.content_version());
        drop(daemons);
    }

    /// A fetch made while the worker restarted (an update) goes on over the next link the
    /// domain dials, and the file comes down whole: the link the domain held is found gone by
    /// the fetch itself, which nothing else would dial again.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_fetch_goes_on_once_the_worker_is_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemons = daemons(dir.path()).await;
        let big: Vec<u8> = (0..6_000_000_u32).map(|i| (i % 239) as u8).collect();
        std::fs::write(daemons.home.join("big.bin"), &big).unwrap();
        let (domain, _heard) =
            domain(&dir.path().join("shared"), daemons.id, &daemons.addr.to_string());
        domain.list(ROOT).await.unwrap();
        daemons.restart_worker(dir.path()).await;

        let into = dir.path().join("fetched");
        std::fs::create_dir_all(&into).unwrap();
        let fetched = tokio::time::timeout(STEP, domain.fetch("big.bin", &into, XferId::new()));
        let (landed, item) = fetched.await.expect("fetched within the step").unwrap();
        assert!(std::fs::read(&landed).unwrap() == big, "whole, byte for byte");
        assert_eq!(item.size, big.len() as u64);
        drop(daemons);
    }

    /// A worker's file copied there and written to a client's pasteboard as its URL in the
    /// worker's place (`slopty_client::clip::Place`) is the domain's item at that path: the
    /// read a paste in Finder makes brings down its bytes.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_copied_files_url_in_the_place_is_the_domains_file() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let (domain, _heard) =
            domain(&dir.path().join("shared"), daemons.id, &daemons.addr.to_string());
        let root = dir.path().join("CloudStorage/Slopty-studio");
        let home = daemons.home.to_string_lossy().into_owned();
        let place = slopty_client::clip::Place { home, root: root.clone() };
        let copied = slopty_client::clip::file_url(&daemons.home.join("src/main.rs"));
        let pasted = place.urls_of(copied.as_bytes()).expect("in the home");
        let urls = slopty_client::clip::file_url_paths(&pasted);
        let [at] = urls.as_slice() else { panic!("one URL: {urls:?}") };
        let id = at.strip_prefix(&root).unwrap().to_str().unwrap().to_owned();
        assert_eq!(id, "src/main.rs");

        let into = dir.path().join("fetched");
        std::fs::create_dir_all(&into).unwrap();
        let (landed, item) = domain.fetch(&id, &into, XferId::new()).await.unwrap();
        assert_eq!(std::fs::read(&landed).unwrap(), b"fn main() {}\n");
        assert_eq!((item.id.as_str(), item.parent.as_str()), ("src/main.rs", "src"));
        drop(daemons);
    }

    /// Whether `changes` updates the item `id`.
    fn updated(changes: &[Change], id: &str) -> bool {
        changes.iter().any(|c| matches!(c, Change::Updated(i) if i.id == id))
    }

    /// The changes since `anchor` once one of the domain's signals finds `wanted` among them.
    async fn heard_until(
        domain: &Domain,
        heard: &Notify,
        anchor: &[u8],
        wanted: impl Fn(&[Change]) -> bool,
    ) -> Option<Vec<Change>> {
        loop {
            heard.notified().await;
            let (changes, _read) = domain.since(anchor).unwrap();
            if wanted(&changes) {
                return Some(changes);
            }
        }
    }

    /// Wait until the worker watches each folder of `folders`, in `home`: the worker arms a
    /// watch on its own time after the listing that asked for it, so a probe is made in each
    /// until one is heard from every folder.
    async fn armed(domain: &Domain, heard: &Notify, home: &Path, folders: &[&str]) {
        for n in 0_u32.. {
            let anchor = domain.anchor();
            let probe = format!(".probe-{n}");
            let ids: Vec<String> = folders
                .iter()
                .map(|folder| {
                    std::fs::write(home.join(folder).join(&probe), b"").unwrap();
                    slopty_files::item::child(folder, &probe).unwrap()
                })
                .collect();
            let all = |changes: &[Change]| ids.iter().all(|id| updated(changes, id));
            let wait = tokio::time::timeout(
                Duration::from_secs(1),
                heard_until(domain, heard, &anchor, all),
            );
            if wait.await.is_ok() {
                return;
            }
            assert!(n < 30, "the worker never watched {folders:?}");
        }
    }

    /// Once the system was given a folder, a file made in it, one changed and one removed on
    /// the worker come to the working set: the domain signals, and the changes since the
    /// anchor before name each of them, once.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_change_on_the_worker_reaches_the_working_set() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let (domain, heard) =
            domain(&dir.path().join("shared"), daemons.id, &daemons.addr.to_string());
        domain.list(ROOT).await.unwrap();
        domain.list("src").await.unwrap();
        armed(&domain, &heard, &daemons.home, &[ROOT, "src"]).await;
        let before = domain.anchor();

        std::fs::write(daemons.home.join("b.txt"), b"new").unwrap();
        std::fs::remove_file(daemons.home.join("a.txt")).unwrap();
        std::fs::write(daemons.home.join("src/lib.rs"), b"pub fn f() {}\n").unwrap();
        let wanted = |changes: &[Change]| {
            updated(changes, "b.txt")
                && updated(changes, "src/lib.rs")
                && changes.contains(&Change::Deleted("a.txt".to_owned()))
        };
        let changes = tokio::time::timeout(STEP, heard_until(&domain, &heard, &before, wanted))
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("changes so far: {:?}", domain.since(&before)));
        assert!(!updated(&changes, "a.txt"), "{changes:?}");
        let (_, read) = domain.since(&before).unwrap();
        assert!(domain.since(&read).unwrap().0.is_empty(), "read once");
        drop(daemons);
    }

    /// A missing item, a file asked to list, a worker the app no longer names, another worker
    /// at the address written for this one, and an address nobody answers each say so as the
    /// error the system is told.
    #[tokio::test(flavor = "multi_thread")]
    async fn what_cannot_be_reached_or_is_not_there_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let addr = daemons.addr.to_string();
        let (domain, _heard) = domain(&dir.path().join("shared"), daemons.id, &addr);
        let missing = domain.item("missing.txt").await.unwrap_err();
        assert!(matches!(missing, FilesError::NoSuchItem(_)), "{missing:?}");
        let file = domain.list("a.txt").await.unwrap_err();
        assert!(matches!(file, FilesError::NotFolder(_)), "{file:?}");

        let forgotten = Domain::new(WorkerId::new(), dir.path().join("shared"), Arc::new(|| {}));
        let forgotten = forgotten.item(ROOT).await.unwrap_err();
        assert!(matches!(forgotten, FilesError::Unreachable(_)), "{forgotten:?}");

        let (other, _heard) = domain_elsewhere(dir.path(), "other", WorkerId::new(), &addr);
        let other = other.item(ROOT).await.unwrap_err();
        assert!(matches!(other, FilesError::WrongWorker { .. }), "{other:?}");

        let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let nobody = silent.local_addr().unwrap().to_string();
        let (nobody, _heard) = domain_elsewhere(dir.path(), "nobody", daemons.id, &nobody);
        let unanswered = nobody.item(ROOT).await.unwrap_err();
        assert!(matches!(unanswered, FilesError::Unreachable(_)), "{unanswered:?}");
        drop((daemons, silent));
    }

    /// [`domain`] in a shared container of its own, `name`, under `dir`.
    fn domain_elsewhere(dir: &Path, name: &str, id: WorkerId, addr: &str) -> (Domain, Arc<Notify>) {
        domain(&dir.join(name), id, addr)
    }
}
