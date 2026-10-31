//! Socket server: one task per connection, shared session table; and the handover of every
//! session to a new build run in place ([`PtydRequest::Succeed`]).

use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result};
use parking_lot::Mutex;
use slopty_core::SessionId;
use slopty_proto::codec;
use slopty_proto::ptyd::PtydError;
use slopty_pty::fdpass;
use slopty_pty::protocol::{
    Bequest, Heir, MAX_CHECKPOINT_BYTES, PtydEvent, PtydRequest, inherit_args,
};
use slopty_pty::shell_integration::ShellIntegration;
use slopty_pty::succession::{exec_in_place, keep_across_exec};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::session::{Broadcast, Session};

/// How long a closed session's child has to exit after its hangup before it is killed.
const HANGUP_GRACE: Duration = Duration::from_secs(3);

/// The first wait after a failed accept, doubled on each one after it up to
/// [`ACCEPT_BACKOFF_MAX`]. A failed accept (out of descriptors, above all) is the moment's, and
/// ending the daemon over it would end every session.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// The longest wait after a failed accept.
const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);

/// How long a new build has to say its custody before a handover to it is given up.
const ASK_CUSTODY: Duration = Duration::from_secs(10);

/// How long every reader has to step out of its master before a handover is given up.
const PARK: Duration = Duration::from_secs(5);

/// The first wait after a failed bind under a handover, doubled on each one after it up to
/// [`ACCEPT_BACKOFF_MAX`]: a ptyd that took sessions over never ends over it.
const BIND_AGAIN: Duration = Duration::from_millis(50);

/// What the custody file says in place of a custody while this ptyd runs the next build.
pub const HANDING: &str = "handing";

/// How often a custody that could not be said is said again.
const SAY_AGAIN: Duration = Duration::from_secs(1);

/// Everything shared between connections.
struct State {
    sessions: Mutex<HashMap<SessionId, Arc<Session>>>,
    events: broadcast::Sender<Broadcast>,
    backlog_bytes: usize,
    /// The shell integration scripts, when they could be written.
    integration: Option<ShellIntegration>,
    next_conn: AtomicU64,
    shutdown: tokio::sync::Notify,
    /// Handovers asked for, for the accept loop, which holds the listener.
    succeed: mpsc::UnboundedSender<Succession>,
    /// A handover is under way: no session is spawned, adopted, closed, attached or reclaimed
    /// until it ends, so the sessions it hands are the ones there are, as they are. Changed
    /// under the lock of `sessions`, and read under it by every request it refuses.
    handing: AtomicBool,
}

/// A handover a connection asked for: the new build, and where to say why it did not happen.
struct Succession {
    program: PathBuf,
    failed: oneshot::Sender<String>,
}

/// How the daemon runs.
#[derive(Debug)]
pub struct Config {
    /// Where it listens.
    pub socket: PathBuf,
    /// Bytes of output kept per session.
    pub backlog_bytes: usize,
    /// Where the shell integration scripts go.
    pub shell_dir: PathBuf,
    /// This build's custody, said beside the socket ([`say_custody`]) while it serves.
    pub custody: &'static str,
    /// This build's succession, said beside it.
    pub succession: &'static str,
    /// The descriptor of the state file the ptyd before this build handed every session over
    /// in ([`PtydRequest::Succeed`]), when this process is that ptyd run anew.
    pub inherit: Option<i32>,
    /// The descriptor of the socket that ptyd listened on, kept open across the `exec` for this
    /// build to serve on.
    pub listener: Option<i32>,
}

/// Bind and serve until `Shutdown`; first, under a handover, take every session handed over and
/// the listener to serve on.
pub async fn run(config: Config) -> Result<()> {
    let (config, kept, heirs) = match config.inherit {
        Some(state) => {
            let Inheritance { bequest, listener, heirs } = inherit(state, config.listener);
            let max = slopty_pty::protocol::MAX_BACKLOG_BYTES;
            let config = if let Some(b) = bequest {
                Config {
                    socket: b.socket,
                    backlog_bytes: usize::try_from(b.backlog_bytes).unwrap_or(max).min(max),
                    shell_dir: b.shell_dir,
                    ..config
                }
            } else {
                // Where it listened, which is where workers dial, its listener still says.
                let bound = listener.as_ref().and_then(|l| l.local_addr().ok());
                let socket = bound
                    .as_ref()
                    .and_then(std::os::unix::net::SocketAddr::as_pathname)
                    .map_or_else(|| config.socket.clone(), Path::to_path_buf);
                tracing::error!(socket = %socket.display(), "the handover says nothing of how the ptyd before ran; serving on its socket, the rest as this command line says");
                Config { socket, ..config }
            };
            (config, listener, heirs)
        }
        None => (config, None, Vec::new()),
    };
    let integration = match slopty_pty::shell_integration::install(&config.shell_dir) {
        Ok(si) => {
            tracing::info!(dir = %si.zdotdir.display(), enabled = si.enabled, "shell integration installed");
            Some(si)
        }
        Err(e) => {
            tracing::warn!(dir = %config.shell_dir.display(), error = %e, "shell integration not installed");
            None
        }
    };
    // ghostty's terminfo, so the shells we spawn can be told `TERM=xterm-ghostty`. Off the
    // critical path: `tic` takes a moment, and `default_term` reads the database per spawn, so
    // a shell that starts before this lands simply gets `xterm-256color`.
    let database = slopty_pty::terminfo::user_database();
    tokio::spawn(async move {
        match slopty_pty::terminfo::install(&database).await {
            Ok(slopty_pty::terminfo::Installed::Already) => {
                tracing::debug!(db = %database.display(), "terminfo already installed");
            }
            Ok(slopty_pty::terminfo::Installed::Compiled) => {
                tracing::info!(db = %database.display(), "terminfo installed");
            }
            Err(e) => {
                tracing::warn!(db = %database.display(), error = %e, "terminfo not installed");
            }
        }
    });
    let (events, _) = broadcast::channel(256);
    let mut sessions = HashMap::new();
    for (heir, master) in heirs {
        let id = heir.id;
        match Session::inherit(heir, master, config.backlog_bytes, events.clone()) {
            Ok(session) => {
                sessions.insert(id, session);
            }
            Err(e) => tracing::error!(session = %id, error = %e, "a handed session not taken"),
        }
    }
    let socket = config.socket.as_path();
    let said = custody_file(socket);
    // Under a handover this build serves on the listener the build before kept open across the
    // `exec` for it, never binding the path afresh: a worker that dialled at any moment of the
    // handover waits in its queue and is served here. The custody comes first then: the pid is
    // the one the file named for the build before, marked `handing`, which no worker takes for
    // one that speaks its custody, so this build's words are there before anything is served. A
    // ptyd that starts afresh says it once bound, so it never writes over another one's.
    let listener = if config.inherit.is_some() {
        say_custody_until_said(said.clone(), config.custody, config.succession);
        match kept.map(|fd| serve_kept(fd, socket)) {
            Some(Ok(listener)) => listener,
            Some(Err(e)) => {
                tracing::error!(error = %format!("{e:#}"), "the listener kept across the exec does not serve; binding afresh");
                bind_until_bound(socket).await
            }
            None => bind_until_bound(socket).await,
        }
    } else {
        let listener = bind(socket).await?;
        say_custody_until_said(said.clone(), config.custody, config.succession);
        listener
    };
    tracing::info!(path = %socket.display(), pid = std::process::id(), sessions = sessions.len(), "slopty-ptyd listening");
    let (succeeding, mut successions) = mpsc::unbounded_channel();
    let state = Arc::new(State {
        sessions: Mutex::new(sessions),
        events,
        backlog_bytes: config.backlog_bytes,
        integration,
        next_conn: AtomicU64::new(1),
        shutdown: tokio::sync::Notify::new(),
        succeed: succeeding,
        handing: AtomicBool::new(false),
    });

    let mut backoff = ACCEPT_BACKOFF;
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    backoff = ACCEPT_BACKOFF;
                    fdpass::widen_buffers(&stream);
                    let st = Arc::clone(&state);
                    tokio::spawn(async move {
                        let id = st.next_conn.fetch_add(1, Ordering::Relaxed);
                        if let Err(e) = Connection::new(id, stream, st).serve().await {
                            tracing::debug!(conn = id, error = %e, "connection ended");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, wait_ms = backoff.as_millis(), "accept failed; trying again");
                    tokio::time::sleep(backoff).await;
                    backoff = backoff.saturating_mul(2).min(ACCEPT_BACKOFF_MAX);
                }
            },
            // No dial is accepted while it runs: each waits in the listener's queue, for this
            // build when the handover fails, for the next one when it happens.
            Some(asked) = successions.recv() => {
                let why = succeed(&state, &config, &asked.program, listener.as_fd()).await;
                tracing::error!(program = %asked.program.display(), error = %format!("{why:#}"), "every session kept here: the handover did not happen");
                let _gone = asked.failed.send(format!("{why:#}"));
            }
            () = state.shutdown.notified() => break,
        }
    }

    tracing::info!("shutting down: hanging up every session");
    let sessions: Vec<Arc<Session>> = state.sessions.lock().values().cloned().collect();
    for s in sessions {
        let _ignored = s.hang_up();
    }
    let _ignored = std::fs::remove_file(socket);
    let _ignored = std::fs::remove_file(&said);
    Ok(())
}

/// Hand every session and the `listener` to the build at `program`, run in place; returns only
/// when that did not happen, with why, everything as it was.
async fn succeed(
    state: &State,
    config: &Config,
    program: &Path,
    listener: BorrowedFd<'_>,
) -> anyhow::Error {
    // The program checked is the one run: `execv` resolves a relative path against the working
    // directory, where the check's spawn would search `PATH`.
    if !program.is_absolute() {
        return anyhow::anyhow!("{} is no absolute path", program.display());
    }
    let said = match ask_custody(program).await {
        Ok(said) => said,
        Err(e) => return e,
    };
    if said.succession != config.succession {
        return anyhow::anyhow!(
            "{} hands sessions on another way (succession {}, this build's {})",
            program.display(),
            said.succession,
            config.succession
        );
    }
    // From here no request changes which sessions there are, or who holds one.
    let sessions: Vec<Arc<Session>> = {
        let sessions = state.sessions.lock();
        state.handing.store(true, Ordering::SeqCst);
        sessions.values().cloned().collect()
    };
    let parked = tokio::time::timeout(PARK, async {
        for session in &sessions {
            session.park().await;
        }
    })
    .await;
    let why = match parked {
        // Written and run in one synchronous step: on this one thread no task runs meanwhile, so
        // no child is reaped, no byte read and no request served between the record of a
        // session and the `exec`.
        Ok(()) => match hand_over(config, program, &sessions, listener) {
            Err(why) => why,
        },
        Err(_late) => {
            anyhow::anyhow!("a session's reader did not stop within {} s", PARK.as_secs())
        }
    };
    {
        let _sessions = state.sessions.lock();
        state.handing.store(false, Ordering::SeqCst);
    }
    // Every session no connection holds drains again, whether this paused it or a request that
    // gave it up while the handover ran did.
    for session in state.sessions.lock().values() {
        session.resume_unless_held();
    }
    why
}

/// The handover itself, once every reader is out of its master: the sessions' state into an
/// unlinked file, it, the listener and every master kept open across `exec`, and this process
/// running `program` with [`inherit_args`]. Whatever was changed for it is put back when it
/// fails.
///
/// The listener goes across rather than closing at the `exec` for the next build to bind the
/// path afresh. Closed, it left a moment in which a dial was refused, and on macOS one in which
/// it still answered: XNU finishes closing an image's close-on-exec descriptors only after the
/// next image runs, so under load the next build found the path taken and backed off, and a
/// dial the dying listener took in was dropped (`docs/decisions/workers.md`).
fn hand_over(
    config: &Config,
    program: &Path,
    sessions: &[Arc<Session>],
    listener: BorrowedFd<'_>,
) -> Result<std::convert::Infallible> {
    let path = config.socket.with_extension("succession");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("create {}", path.display()))?;
    // Named by its descriptor alone from here: nothing is left behind, however this ends.
    std::fs::remove_file(&path).with_context(|| format!("unlink {}", path.display()))?;
    let bequest = Bequest {
        socket: config.socket.clone(),
        backlog_bytes: u64::try_from(config.backlog_bytes).unwrap_or(u64::MAX),
        shell_dir: config.shell_dir.clone(),
        sessions: u32::try_from(sessions.len()).context("too many sessions")?,
        fds: sessions.iter().map(|session| session.master_fd().as_raw_fd()).collect(),
    };
    {
        let mut out = std::io::BufWriter::new(&mut file);
        out.write_all(&codec::encode(&bequest)?).context("write the handover")?;
        for session in sessions {
            out.write_all(&codec::encode(&session.heir())?).context("write the handover")?;
        }
        out.flush().context("write the handover")?;
    }
    file.rewind().context("rewind the handover")?;
    let kept: Vec<BorrowedFd<'_>> = [file.as_fd(), listener]
        .into_iter()
        .chain(sessions.iter().map(|session| session.master_fd()))
        .collect();
    let mut failed = None;
    for fd in &kept {
        if let Err(e) = keep_across_exec(*fd, true) {
            failed = Some(anyhow::Error::new(e).context("keep a descriptor across exec"));
            break;
        }
    }
    let failed = failed.unwrap_or_else(|| {
        tracing::info!(program = %program.display(), sessions = sessions.len(), "handing every session to the new build");
        // Until the new build says its own, no worker of this build's custody takes this pid
        // for one that speaks it.
        let said = custody_file(&config.socket);
        let pid = std::process::id();
        if let Err(e) = say_custody(&said, pid, HANDING, config.succession) {
            tracing::warn!(error = %e, "custody not marked as handing");
        }
        let why = exec_in_place(program, &inherit_args(file.as_raw_fd(), listener.as_raw_fd()));
        if let Err(e) = say_custody(&said, pid, config.custody, config.succession) {
            tracing::warn!(error = %e, "custody not said again");
        }
        anyhow::Error::new(why).context(format!("run {}", program.display()))
    });
    for fd in kept {
        if let Err(e) = keep_across_exec(fd, false) {
            // Only a shell this ptyd starts later would get it, and only by a fork of ours,
            // which closes every descriptor it does not hand on.
            tracing::warn!(fd = fd.as_raw_fd(), error = %e, "a descriptor left open across exec");
        }
    }
    Err(failed)
}

/// What the ptyd before this build handed over ([`inherit`]).
struct Inheritance {
    /// How it ran; `None` when the state file does not say.
    bequest: Option<Bequest>,
    /// The socket it listened on, kept open across the `exec`; `None` when it did not come.
    listener: Option<std::os::unix::net::UnixListener>,
    /// Every session that came whole, with its master.
    heirs: Vec<(Heir, OwnedFd)>,
}

/// Every session the ptyd before this build handed over in the state file on descriptor
/// `state`, with its master, how that ptyd ran, and the socket it listened on, on descriptor
/// `listener`. As much as came whole: a session whose record or master is missing is left out,
/// and the rest kept, never ended. The listener is taken before anything is read, so a state
/// that does not read leaves it served all the same, and every descriptor the [`Bequest`] names
/// is taken before any heir is read, so one whose heir does not read is closed rather than left
/// open, unowned, for every program this process starts.
fn inherit(state: i32, listener: Option<i32>) -> Inheritance {
    let mut heirs = Vec::new();
    let mut taken = HashSet::new();
    let mut take = |raw: i32| {
        if !taken.insert(raw) {
            return Err(std::io::Error::other(format!("descriptor {raw} named twice")));
        }
        #[expect(unsafe_code, reason = "owning the descriptors the image before kept for this one")]
        // SAFETY: `raw` comes from the handover the image before this one made (its command line
        // and state file), which kept each descriptor it names open across the `exec` for this
        // image (`hand_over`); this runs first, before this process opens a PTY or receives a
        // descriptor, and `taken` lets no number be taken twice, so nothing else owns it.
        let fd = unsafe { slopty_pty::succession::take_inherited(raw) };
        fd
    };
    let listener = listener.and_then(|raw| match take(raw) {
        Ok(fd) => Some(std::os::unix::net::UnixListener::from(fd)),
        Err(e) => {
            tracing::error!(fd = raw, error = %e, "the listener did not come across");
            None
        }
    });
    let file = match take(state) {
        Ok(file) => file,
        Err(e) => {
            tracing::error!(fd = state, error = %e, "the handover's state is not there");
            return Inheritance { bequest: None, listener, heirs };
        }
    };
    let mut frames = Frames::new(std::fs::File::from(file));
    let bequest = match frames.next::<Bequest>() {
        Ok(Some(bequest)) => bequest,
        Ok(None) => {
            tracing::error!("the handover's state is empty");
            return Inheritance { bequest: None, listener, heirs };
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "the handover's state does not read");
            return Inheritance { bequest: None, listener, heirs };
        }
    };
    let mut masters: HashMap<i32, OwnedFd> = HashMap::new();
    for &raw in &bequest.fds {
        match take(raw) {
            Ok(fd) => {
                masters.insert(raw, fd);
            }
            Err(e) => tracing::error!(fd = raw, error = %e, "a handed descriptor not taken"),
        }
    }
    for _ in 0..bequest.sessions {
        let heir = match frames.next::<Heir>() {
            Ok(Some(heir)) => heir,
            Ok(None) => {
                tracing::error!(taken = heirs.len(), "the handover's state ends early");
                break;
            }
            Err(e) => {
                tracing::error!(taken = heirs.len(), error = %format!("{e:#}"), "a handed session does not read");
                break;
            }
        };
        match masters.remove(&heir.master) {
            Some(master) => heirs.push((heir, master)),
            None => {
                tracing::error!(session = %heir.id, fd = heir.master, "a handed session came without its master");
            }
        }
    }
    if !masters.is_empty() {
        tracing::error!(closed = masters.len(), "masters handed over with no session that reads");
    }
    Inheritance { bequest: Some(bequest), listener, heirs }
}

/// The listener the build before kept open across the `exec`, to serve on once it is one: a
/// socket bound to `socket`, taken by this process's runtime.
fn serve_kept(listener: std::os::unix::net::UnixListener, socket: &Path) -> Result<UnixListener> {
    let bound = listener.local_addr().context("the kept descriptor is no socket")?;
    anyhow::ensure!(
        bound.as_pathname() == Some(socket),
        "the kept listener is bound to {bound:?}, not {}",
        socket.display()
    );
    listener.set_nonblocking(true).context("the kept listener")?;
    UnixListener::from_std(listener).context("serve on the kept listener")
}

/// Frames read one by one off a file, never more of it at once than the frame being read.
struct Frames {
    file: std::fs::File,
    buf: bytes::BytesMut,
    chunk: Box<[u8]>,
}

impl Frames {
    fn new(file: std::fs::File) -> Self {
        Self { file, buf: bytes::BytesMut::new(), chunk: vec![0; 64 << 10].into_boxed_slice() }
    }

    /// The next frame; `None` at the end of the file.
    fn next<T: serde::de::DeserializeOwned>(&mut self) -> Result<Option<T>> {
        loop {
            if let Some(frame) = codec::try_decode::<T>(&mut self.buf)? {
                return Ok(Some(frame));
            }
            let n = self.file.read(&mut self.chunk).context("read the handover")?;
            if n == 0 {
                anyhow::ensure!(self.buf.is_empty(), "the handover ends inside a frame");
                return Ok(None);
            }
            self.buf.extend_from_slice(self.chunk.get(..n).unwrap_or_default());
        }
    }
}

/// What `slopty-ptyd --custody` of `program` says.
async fn ask_custody(program: &Path) -> Result<Said> {
    let asked = tokio::process::Command::new(program)
        .arg("--custody")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(ASK_CUSTODY, asked)
        .await
        .with_context(|| format!("{} --custody took too long", program.display()))?
        .with_context(|| format!("run {} --custody", program.display()))?;
    anyhow::ensure!(out.status.success(), "{} --custody: {}", program.display(), out.status);
    let text = String::from_utf8_lossy(&out.stdout);
    let mut words = text.split_whitespace();
    match (words.next(), words.next()) {
        (Some(custody), Some(succession)) => {
            Ok(Said { pid: 0, custody: custody.to_owned(), succession: succession.to_owned() })
        }
        _ => anyhow::bail!("{} --custody says no succession: {text:?}", program.display()),
    }
}

/// What a ptyd says of itself beside its socket: `<pid> <custody> <succession>`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Said {
    /// The ptyd that wrote it.
    pub pid: u32,
    /// Its custody fingerprint.
    pub custody: String,
    /// Its succession fingerprint.
    pub succession: String,
}

impl Said {
    /// What the custody file at `path` says, when it says all of it.
    pub fn read(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut words = text.split_whitespace();
        let pid = words.next()?.parse().ok()?;
        let custody = words.next()?.to_owned();
        let succession = words.next()?.to_owned();
        Some(Self { pid, custody, succession })
    }
}

/// Where the daemon on `socket` says its custody: beside it, `ptyd.sock` → `ptyd.custody`
/// (`slopty_platform::service::Layout::ptyd_custody`).
pub fn custody_file(socket: &Path) -> PathBuf {
    socket.with_extension("custody")
}

/// Write `<pid> <custody> <succession>` to `path`, whole or not at all: an install reads it to
/// tell whether the new build can keep this daemon and its sessions, or be handed them, and
/// trusts it only while `pid` is the process its service manager runs. A worker reads it to
/// tell whether it speaks this daemon's protocol.
fn say_custody(path: &Path, pid: u32, custody: &str, succession: &str) -> std::io::Result<()> {
    let part = path.with_extension("custody.part");
    std::fs::write(&part, format!("{pid} {custody} {succession}\n"))?;
    std::fs::rename(&part, path)
}

/// Say this build's custody beside the socket ([`say_custody`]), and keep trying each
/// [`SAY_AGAIN`] while that fails: a worker dials only a ptyd that says it, and this one serves
/// on meanwhile, ending no session over it.
fn say_custody_until_said(path: PathBuf, custody: &'static str, succession: &'static str) {
    let pid = std::process::id();
    let Err(e) = say_custody(&path, pid, custody, succession) else { return };
    tracing::warn!(path = %path.display(), error = %e, "custody not written; trying again");
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SAY_AGAIN).await;
            if say_custody(&path, pid, custody, succession).is_ok() {
                tracing::info!(path = %path.display(), "custody written");
                return;
            }
        }
    });
}

/// Bind as [`bind`] does, trying again until it can: under a handover whose listener did not
/// come across, ending would end every session handed over, which go on draining meanwhile.
async fn bind_until_bound(socket: &Path) -> UnixListener {
    let mut wait = BIND_AGAIN;
    loop {
        match bind(socket).await {
            Ok(listener) => return listener,
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), wait_ms = wait.as_millis(), "not listening yet; every session kept");
                tokio::time::sleep(wait).await;
                wait = wait.saturating_mul(2).min(ACCEPT_BACKOFF_MAX);
            }
        }
    }
}

/// Create the socket directory (0700) and bind, replacing a socket file nothing answers on, or
/// one only this very process answers on. That one is no other ptyd but this process before an
/// `exec`: an image whose listener XNU has not finished closing (it does so only after the next
/// image runs), or a listener a handover kept open that did not come across as one. Another
/// process that answers is another ptyd, and is left alone.
async fn bind(socket: &Path) -> Result<UnixListener> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        rustix::fs::chmod(dir, rustix::fs::Mode::RWXU)
            .with_context(|| format!("chmod {}", dir.display()))?;
    }
    if socket.exists() {
        match UnixStream::connect(socket).await {
            Ok(probe) => {
                // The listener's process: the one that listened, which an `exec` keeps.
                let peer = probe.peer_cred().ok().and_then(|cred| cred.pid());
                let this = i32::try_from(std::process::id()).ok();
                if peer.is_none() || peer != this {
                    let pid = peer.map_or_else(|| "unknown".to_owned(), |pid| pid.to_string());
                    anyhow::bail!(
                        "another slopty-ptyd (pid {pid}) is already listening on {}",
                        socket.display()
                    );
                }
                tracing::warn!(path = %socket.display(), "replacing a listener this process kept from before its exec");
            }
            Err(_refused) => tracing::warn!(path = %socket.display(), "removing stale socket"),
        }
        std::fs::remove_file(socket).context("remove stale socket")?;
    }
    let listener =
        UnixListener::bind(socket).with_context(|| format!("bind {}", socket.display()))?;
    rustix::fs::chmod(socket, rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR)
        .context("chmod socket")?;
    Ok(listener)
}

struct Connection {
    id: u64,
    stream: UnixStream,
    state: Arc<State>,
    inbox: fdpass::Inbox,
    attached: HashSet<SessionId>,
}

/// Whatever a connection held goes back to ptyd's care when it goes, however it went: the
/// worker hung up, died mid-reply, or sent a frame that does not decode. A session left claimed
/// would refuse every later attach, and one left paused would stop draining its child.
impl Drop for Connection {
    fn drop(&mut self) {
        for id in std::mem::take(&mut self.attached) {
            let session = self.state.sessions.lock().get(&id).cloned();
            if let Some(s) = session {
                s.release(self.id);
                s.resume_reader();
            }
        }
    }
}

impl Connection {
    fn new(id: u64, stream: UnixStream, state: Arc<State>) -> Self {
        Self { id, stream, state, inbox: fdpass::Inbox::default(), attached: HashSet::new() }
    }

    /// Serve until the worker hangs up or the connection fails. However it ends, dropping the
    /// connection hands back what it held (see its `Drop`).
    async fn serve(mut self) -> Result<()> {
        let mut events = self.state.events.subscribe();
        loop {
            tokio::select! {
                read = self.inbox.recv(&self.stream) => {
                    if read? == 0 {
                        return Ok(());
                    }
                    while let Some(req) = self.inbox.decode::<PtydRequest>()? {
                        self.handle(req).await?;
                    }
                    // Only `Adopt` brings a descriptor, and takes it. Any other is a stray,
                    // closed once no frame is part way in that it could still belong to.
                    if self.inbox.idle() {
                        self.inbox.close_fds();
                    }
                }
                ev = events.recv() => match ev {
                    Ok(Broadcast::Exited { id, exit }) => {
                        self.reply(&PtydEvent::Exited { id, exit }, None).await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(conn = self.id, lagged = n, "event stream lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }

    async fn reply(&self, ev: &PtydEvent, fd: Option<BorrowedFd<'_>>) -> Result<()> {
        let frame = codec::encode(ev)?;
        fdpass::send(&self.stream, &frame, fd).await?;
        Ok(())
    }

    async fn error(&self, id: Option<SessionId>, error: PtydError) -> Result<()> {
        self.reply(&PtydEvent::Error { id, error }, None).await
    }

    /// A handover is under way. Each request it refuses goes on to its change with no `await`
    /// between, or asks again after its one `await` (an attach or a reclaim waiting for the
    /// reader), and this daemon runs on one thread, so a handover begins either before the
    /// check (and the request is refused) or after the change (and hands its outcome on).
    fn handing(&self) -> bool {
        let _sessions = self.state.sessions.lock();
        self.state.handing.load(Ordering::SeqCst)
    }

    /// A handover began while this connection waited to take session `id`: it lets go of it,
    /// for the handover to hand on (or, when that fails, to drain again), and says why.
    async fn give_up(&mut self, id: SessionId, session: &Session) -> Result<()> {
        session.release(self.id);
        self.attached.remove(&id);
        self.error(Some(id), handing()).await
    }

    fn session(&self, id: SessionId) -> Option<Arc<Session>> {
        self.state.sessions.lock().get(&id).cloned()
    }

    async fn handle(&mut self, req: PtydRequest) -> Result<()> {
        match req {
            PtydRequest::Spawn { id, spec } => {
                if self.handing() {
                    return self.error(Some(id), handing()).await;
                }
                if self.session(id).is_some() {
                    return self.error(Some(id), PtydError::SessionExists).await;
                }
                match Session::spawn(
                    id,
                    &spec,
                    self.state.backlog_bytes,
                    self.state.integration.as_ref(),
                    self.state.events.clone(),
                ) {
                    Ok(session) => {
                        let info = session.info();
                        tracing::info!(session = %id, pid = info.pid, tty = %info.tty.display(), "spawned");
                        self.state.sessions.lock().insert(id, session);
                        self.reply(&PtydEvent::Spawned { id, pid: info.pid }, None).await
                    }
                    Err(e) => self.error(Some(id), PtydError::Os(e.to_string())).await,
                }
            }
            PtydRequest::Attach { id } => {
                if self.handing() {
                    return self.error(Some(id), handing()).await;
                }
                let Some(session) = self.session(id) else {
                    return self.error(Some(id), PtydError::NoSuchSession).await;
                };
                if !session.claim(self.id) {
                    return self.error(Some(id), PtydError::AttachedElsewhere).await;
                }
                // Held from the claim on, so the connection's drop lets go of it whatever
                // happens from here.
                self.attached.insert(id);
                session.park().await;
                if self.handing() {
                    return self.give_up(id, &session).await;
                }
                let handed = session.hand_over();
                let ev = PtydEvent::Attached {
                    id,
                    checkpoint: handed.checkpoint,
                    backlog: handed.backlog,
                    dropped: handed.dropped,
                    size: handed.size,
                    started_ms: handed.started_ms,
                    term: handed.term,
                };
                self.reply(&ev, Some(session.master_fd())).await
            }
            PtydRequest::Output { id, bytes } => {
                // No reply by contract.
                if let Some(session) = self.session(id) {
                    session.tap(self.id, &bytes);
                }
                Ok(())
            }
            PtydRequest::Checkpoint { id, state } => {
                if state.len() > MAX_CHECKPOINT_BYTES {
                    // It and the backlog would not fit the next `Attached`; the ring keeps
                    // everything since the checkpoint that did.
                    tracing::warn!(session = %id, bytes = state.len(), "checkpoint too large; ignored");
                    return Ok(());
                }
                if let Some(session) = self.session(id) {
                    session.set_checkpoint(self.id, state);
                }
                Ok(())
            }
            PtydRequest::Resize { id, size } => {
                // No reply by contract: the worker's taps queue behind it.
                let Some(session) = self.session(id) else {
                    tracing::debug!(session = %id, "resize of no session");
                    return Ok(());
                };
                session.set_size(size);
                if let Err(e) = slopty_pty::pty::set_size(session.master_fd(), size) {
                    tracing::warn!(session = %id, error = %e, "TIOCSWINSZ failed");
                }
                Ok(())
            }
            PtydRequest::Close { id } => {
                if self.handing() {
                    return self.error(Some(id), handing()).await;
                }
                let Some(session) = self.state.sessions.lock().remove(&id) else {
                    return self.error(Some(id), PtydError::NoSuchSession).await;
                };
                self.attached.remove(&id);
                session.close(HANGUP_GRACE);
                self.reply(&PtydEvent::Ok, None).await
            }
            PtydRequest::List => {
                let list = self.state.sessions.lock().values().map(|s| s.info()).collect();
                self.reply(&PtydEvent::Sessions(list), None).await
            }
            PtydRequest::Shutdown => {
                self.reply(&PtydEvent::Ok, None).await?;
                self.state.shutdown.notify_one();
                Ok(())
            }
            PtydRequest::Reclaim { id } => {
                if self.handing() {
                    return self.error(Some(id), handing()).await;
                }
                let Some(session) = self.session(id) else {
                    return self.error(Some(id), PtydError::NoSuchSession).await;
                };
                if !session.claim(self.id) {
                    return self.error(Some(id), PtydError::AttachedElsewhere).await;
                }
                self.attached.insert(id);
                session.park().await;
                if self.handing() {
                    return self.give_up(id, &session).await;
                }
                self.reply(&PtydEvent::Ok, None).await
            }
            PtydRequest::Adopt { id, pid, size, started_ms, term } => {
                // The master rode on this frame, so it is the oldest one queued.
                let Some(master) = self.inbox.take_fd() else {
                    let missing = PtydError::Os("no master came with the session".to_owned());
                    return self.error(Some(id), missing).await;
                };
                if self.handing() {
                    return self.error(Some(id), handing()).await;
                }
                if self.session(id).is_some() {
                    return self.error(Some(id), PtydError::SessionExists).await;
                }
                let child = slopty_pty::Adoptee { pid, size, started_ms, term };
                match Session::adopt(
                    id,
                    master,
                    child,
                    self.state.backlog_bytes,
                    self.id,
                    self.state.events.clone(),
                ) {
                    Ok(session) => {
                        tracing::info!(session = %id, pid, "adopted from a worker");
                        self.state.sessions.lock().insert(id, session);
                        self.attached.insert(id);
                        self.reply(&PtydEvent::Ok, None).await
                    }
                    Err(e) => self.error(Some(id), PtydError::Os(e.to_string())).await,
                }
            }
            PtydRequest::Succeed { program } => {
                let (failed, why) = oneshot::channel();
                let asked = Succession { program, failed };
                if self.state.succeed.send(asked).is_err() {
                    return Ok(());
                }
                // A handover that happens never answers: this process is the new build then.
                match why.await {
                    Ok(why) => self.error(None, PtydError::Os(why)).await,
                    Err(_dropped) => Ok(()),
                }
            }
        }
    }
}

/// Why a request that changes the sessions is refused while a handover is under way.
fn handing() -> PtydError {
    PtydError::Os("slopty-ptyd is handing its sessions to a new build; ask again".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{Duration, UnixStream, bind};

    /// A listener this very process holds on the socket is this process before an `exec` (one
    /// XNU has not finished closing, or one a handover kept that did not come across), never
    /// another ptyd: the socket is taken over, and a dial reaches the new listener.
    #[tokio::test]
    async fn a_listener_this_process_holds_is_no_rival() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ptyd.sock");
        let _before = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let listener = bind(&socket).await.expect("its own listener is no rival");
        let _dial = UnixStream::connect(&socket).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .expect("the dial reaches the new listener")
            .unwrap();
    }
}
