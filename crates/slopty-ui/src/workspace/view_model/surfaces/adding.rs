//! Settings → Devices' Add device, and the server this app follows: the shell's dialog asks, the
//! app runs it.
//!
//! A call hands the app what it is to do through a [`WorkspaceEvent`] and a slot the app takes
//! it from: an [`AddDevice`] run (this Mac shared, a machine installed over ssh, a server
//! connected to or set up over ssh), whose progress comes back as the call's stream and which
//! waits on the person's answers when it stops to ask (a host key to trust, a password, a
//! checklist line's fix, which server to join); or a [`Look`] on the tailnet for servers and
//! machines to offer. Dropping the stream (the dialog closed, Stop) drops the run. A password
//! goes from the dialog to the run's `ssh` in memory and nowhere else.

use std::collections::HashMap;

use futures::StreamExt as _;
use gpui::{Context, Task};
use serde::{Deserialize, Serialize};
use slopty_view_rpc::{RpcError, RpcReply};
use tokio::sync::mpsc;

use crate::workspace::{WorkspaceEvent, WorkspaceView};

/// What an Add device run puts where.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AddKind {
    /// This Mac's worker, registered with this app's server (or one started here).
    ThisMac,
    /// A worker on a machine reached over ssh, registered with this app's server.
    Ssh,
    /// The server at an address, which this app then follows.
    Server,
    /// A server put on a machine reached over ssh, which this app then follows.
    ServeSsh,
}

/// An Add device run the app is to start: what it puts where, where its progress goes, and the
/// person's answers when it stops to ask.
#[derive(Debug)]
pub struct AddDevice {
    /// What it puts where.
    pub kind: AddKind,
    /// The machine or server as typed: an ssh config name, `host`, `user@host`,
    /// `user@host:port`, or a server's `host[:port]`; empty for this Mac.
    pub target: String,
    /// Its progress, each frame the whole of it so far; dropped by the dialog to stop the run.
    pub frames: mpsc::UnboundedSender<AddProgress>,
    /// What the person said when it stopped to ask.
    pub answers: mpsc::UnboundedReceiver<AddAnswer>,
}

/// What the person said to a run that stopped to ask.
pub enum AddAnswer {
    /// Trust the host key it stopped at, and go on.
    Trust,
    /// Sign in with this password, for this run only, and add the person's key there when
    /// `add_key`.
    Password {
        /// The password, held only until the run hands it to `ssh`.
        password: String,
        /// Put the person's public key there, so the machine stops asking.
        add_key: bool,
    },
    /// A step's fix ([`AddFix::id`]).
    Fix(String),
    /// Join this server ([`AddChoice::at`]).
    Choose(String),
}

impl std::fmt::Debug for AddAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trust => f.write_str("Trust"),
            Self::Password { add_key, .. } => {
                f.debug_struct("Password").field("add_key", add_key).finish_non_exhaustive()
            }
            Self::Fix(id) => f.debug_tuple("Fix").field(id).finish(),
            Self::Choose(at) => f.debug_tuple("Choose").field(at).finish(),
        }
    }
}

/// A run's progress as the dialog draws it.
#[derive(Clone, PartialEq, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddProgress {
    /// A line per step, or per line of this Mac's checklist.
    pub steps: Vec<AddStep>,
    /// How much of the copy is done, while it runs.
    pub share: Option<f32>,
    /// Why it stopped, once it has.
    pub failure: Option<AddFailure>,
    /// The servers it asks the person to choose between ([`AddAnswer::Choose`]).
    pub choices: Vec<AddChoice>,
    /// What the run says it did, once done.
    pub added: Option<String>,
}

/// One step of a run.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddStep {
    /// What it does.
    pub title: String,
    /// What it said of itself.
    pub detail: Option<String>,
    /// Where it is.
    pub mark: AddMark,
    /// What the person can do about it, when it waits on them.
    pub fix: Option<AddFix>,
}

/// A step's way on, the person's to press.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddFix {
    /// What the answer names it by.
    pub id: String,
    /// The button's words.
    pub label: String,
}

/// Where a step is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AddMark {
    /// Done.
    Done,
    /// Under way.
    Running,
    /// It stopped here.
    Failed,
    /// It waits on the person (a grant, a choice); its fix says what to do.
    NeedsYou,
    /// Not begun, or not known yet.
    Waiting,
}

/// A server a run offers to join.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddChoice {
    /// Its name on the tailnet.
    pub name: String,
    /// Where it is dialled.
    pub at: String,
}

/// Why a run stopped.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddFailure {
    /// What went wrong.
    pub title: String,
    /// What the person may do about it.
    pub hint: Option<String>,
    /// What the machine said.
    pub lines: Vec<String>,
    /// The host key's fingerprints, when it stopped at a key `ssh` does not know: trusting it
    /// goes on ([`AddAnswer::Trust`]).
    pub trust: Option<Vec<String>>,
    /// Whose password the machine asks for (`user@host`), when it asks: one given goes on
    /// ([`AddAnswer::Password`]).
    pub password: Option<String>,
    /// A password was given and the machine said no.
    pub refused: bool,
}

/// A look on the tailnet the app is to make: what answered goes to `frames`.
#[derive(Debug)]
pub struct Look {
    /// What the look found, each frame the whole of it; dropped by the dialog.
    pub frames: mpsc::UnboundedSender<Found>,
}

/// What a look on the tailnet found.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    /// It is still looking.
    pub looking: bool,
    /// This device's tailnet is up: an empty answer is the tailnet's, not a look never made.
    pub running: bool,
    /// Servers that answered, best first.
    pub servers: Vec<FoundNode>,
    /// Machines with a worker the server does not list.
    pub workers: Vec<FoundNode>,
}

/// A node that answered on the tailnet.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundNode {
    /// Its name on the tailnet.
    pub name: String,
    /// Its tailnet address, which is dialled.
    pub at: String,
    /// It takes this device now; else `note` says why not.
    pub ready: bool,
    /// Why it cannot take this device yet: another build, or the tailnet's policy.
    pub note: Option<String>,
}

/// The Add device runs under way, by the number the dialog answers them with, and what the app
/// is yet to take.
#[derive(Debug, Default)]
pub(super) struct Adding {
    /// The last number given a run.
    last: u64,
    /// Each run's way to the person's answers.
    answers: HashMap<u64, mpsc::UnboundedSender<AddAnswer>>,
    /// The run the app is yet to take.
    pending: Option<AddDevice>,
    /// The look the app is yet to take.
    look: Option<Look>,
}

/// A frame as the dialog reads it, with the run's number for its answers.
fn numbered(run: u64, frame: &AddProgress) -> serde_json::Value {
    let mut value = serde_json::to_value(frame).unwrap_or_default();
    if let Some(fields) = value.as_object_mut() {
        fields.insert("run".to_owned(), run.into());
    }
    value
}

impl WorkspaceView {
    /// Add device: hand the app a run of the kind the call names, and stream its progress, each
    /// frame with the run's number for the answers.
    pub(super) fn add_device(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let kind = params.get("kind").cloned().map(serde_json::from_value::<AddKind>);
        let kind = match kind {
            None => AddKind::Ssh,
            Some(Ok(kind)) => kind,
            Some(Err(e)) => return Task::ready(Err(RpcError::BadParams(format!("kind: {e}")))),
        };
        let target = super::param(params, "target").map(str::trim).unwrap_or_default().to_owned();
        if target.is_empty() && kind != AddKind::ThisMac {
            return Task::ready(Err(RpcError::BadParams("target".to_owned())));
        }
        let (frames, mut progress) = mpsc::unbounded_channel();
        let (answer, answers) = mpsc::unbounded_channel();
        let adding = &mut self.view_model.surfaces.adding;
        adding.answers.retain(|_, answer| !answer.is_closed());
        adding.last = adding.last.wrapping_add(1);
        let run = adding.last;
        adding.answers.insert(run, answer);
        adding.pending = Some(AddDevice { kind, target, frames, answers });
        cx.emit(WorkspaceEvent::AddDevice);
        let stream = futures::stream::poll_fn(move |cx| {
            progress.poll_recv(cx).map(|frame| frame.map(|frame| numbered(run, &frame)))
        })
        .boxed();
        Task::ready(Ok(RpcReply::Stream(stream)))
    }

    /// The person's answer to the run the dialog names.
    pub(super) fn answer_add_device(
        &self,
        params: &serde_json::Value,
    ) -> Result<RpcReply, RpcError> {
        let run = params
            .get("run")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| RpcError::BadParams("run".to_owned()))?;
        let answer = if params.get("trust").and_then(serde_json::Value::as_bool) == Some(true) {
            AddAnswer::Trust
        } else if let Some(fix) = super::param(params, "fix") {
            AddAnswer::Fix(fix.to_owned())
        } else if let Some(at) = super::param(params, "choose") {
            AddAnswer::Choose(at.to_owned())
        } else {
            let password = super::param(params, "password")
                .ok_or_else(|| RpcError::BadParams("trust, fix, choose or password".to_owned()))?;
            let add_key = params.get("addKey").and_then(serde_json::Value::as_bool);
            AddAnswer::Password { password: password.to_owned(), add_key: add_key.unwrap_or(true) }
        };
        let sent = self.view_model.surfaces.adding.answers.get(&run).map(|to| to.send(answer));
        match sent {
            Some(Ok(())) => super::value(&serde_json::json!({})),
            _ => Err(RpcError::Failed("That run is over: start it again".to_owned())),
        }
    }

    /// Look on the tailnet for servers and machines, and stream what answers.
    pub(super) fn look_on_tailnet(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (frames, mut found) = mpsc::unbounded_channel();
        self.view_model.surfaces.adding.look = Some(Look { frames });
        cx.emit(WorkspaceEvent::LookOnTailnet);
        let stream = futures::stream::poll_fn(move |cx| {
            found
                .poll_recv(cx)
                .map(|frame| frame.map(|frame| serde_json::to_value(frame).unwrap_or_default()))
        })
        .boxed();
        Task::ready(Ok(RpcReply::Stream(stream)))
    }

    /// The Add device run the app is to start, once ([`WorkspaceEvent::AddDevice`]).
    pub const fn take_add_device(&mut self) -> Option<AddDevice> {
        self.view_model.surfaces.adding.pending.take()
    }

    /// The look on the tailnet the app is to make, once ([`WorkspaceEvent::LookOnTailnet`]).
    pub const fn take_look(&mut self) -> Option<Look> {
        self.view_model.surfaces.adding.look.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_carries_its_run() {
        let frame =
            AddProgress { added: Some("Added mac-mini".to_owned()), ..AddProgress::default() };
        let value = numbered(7, &frame);
        assert_eq!(value["run"], 7);
        assert_eq!(value["added"], "Added mac-mini");
        assert_eq!(value["steps"], serde_json::json!([]));
    }

    #[test]
    fn the_kinds_read_as_the_dialog_names_them() {
        let kinds: Vec<AddKind> =
            serde_json::from_value(serde_json::json!(["thisMac", "ssh", "server", "serveSsh"]))
                .unwrap();
        assert_eq!(kinds, [AddKind::ThisMac, AddKind::Ssh, AddKind::Server, AddKind::ServeSsh]);
    }
}
