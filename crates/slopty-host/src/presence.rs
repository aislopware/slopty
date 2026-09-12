//! Who is looking where.
//!
//! Each connected client's viewport on the canvas, for the others to draw. Ephemeral by
//! design — nothing here is persisted or versioned with the document; a client that reconnects
//! simply says again where it looks.

use std::collections::BTreeMap;

use slopty_core::ClientId;
use slopty_proto::canvas::{CanvasSync, Rect};
use slopty_proto::handshake::ClientKind;

/// One client's last word on where it looks.
#[derive(Clone, PartialEq, Debug)]
struct Looker {
    kind: ClientKind,
    name: String,
    view: Rect,
}

/// The table of lookers.
#[derive(Default, Debug)]
pub struct Presence {
    lookers: BTreeMap<ClientId, Looker>,
}

impl Presence {
    /// `client` looks at `view` (or away, with `None`). The sync to broadcast, or `None` when
    /// nothing changed — a viewport that settles on the same rect twice is one message.
    pub fn look(
        &mut self,
        client: ClientId,
        kind: ClientKind,
        name: &str,
        view: Option<Rect>,
    ) -> Option<CanvasSync> {
        match view {
            Some(view) => {
                let looker = Looker { kind, name: name.to_owned(), view };
                if self.lookers.get(&client) == Some(&looker) {
                    return None;
                }
                self.lookers.insert(client, looker);
                Some(CanvasSync::Presence { client, kind, name: name.to_owned(), view: Some(view) })
            }
            None => self.leave(client),
        }
    }

    /// `client` is gone: the sync to broadcast, or `None` when it was never looking.
    pub fn leave(&mut self, client: ClientId) -> Option<CanvasSync> {
        let looker = self.lookers.remove(&client)?;
        Some(CanvasSync::Presence { client, kind: looker.kind, name: looker.name, view: None })
    }

    /// Everyone looking right now except `except`, for a client that just connected.
    #[must_use]
    pub fn all_but(&self, except: ClientId) -> Vec<CanvasSync> {
        self.lookers
            .iter()
            .filter(|(client, _)| **client != except)
            .map(|(client, l)| CanvasSync::Presence {
                client: *client,
                kind: l.kind,
                name: l.name.clone(),
                view: Some(l.view),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: Rect = Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 };

    #[test]
    fn a_look_is_told_once_a_leave_is_told_and_a_newcomer_hears_the_rest() {
        let (a, b) = (ClientId::new(), ClientId::new());
        let mut p = Presence::default();
        let told = p.look(a, ClientKind::Mac, "studio", Some(VIEW));
        assert_eq!(
            told,
            Some(CanvasSync::Presence {
                client: a,
                kind: ClientKind::Mac,
                name: "studio".to_owned(),
                view: Some(VIEW)
            })
        );
        assert_eq!(p.look(a, ClientKind::Mac, "studio", Some(VIEW)), None, "unchanged");
        let moved = Rect { x: 10.0, ..VIEW };
        assert!(p.look(a, ClientKind::Mac, "studio", Some(moved)).is_some());
        assert_eq!(p.leave(b), None, "b never looked");
        assert!(p.look(b, ClientKind::IPhone, "phone", Some(VIEW)).is_some());
        assert_eq!(p.all_but(b).len(), 1, "a newcomer hears everyone else");
        assert!(
            matches!(p.all_but(b)[0], CanvasSync::Presence { client, view: Some(v), .. } if client == a && v == moved)
        );
        assert_eq!(
            p.look(b, ClientKind::IPhone, "phone", None),
            Some(CanvasSync::Presence {
                client: b,
                kind: ClientKind::IPhone,
                name: "phone".to_owned(),
                view: None
            }),
            "looking away is a leave"
        );
        assert_eq!(p.leave(b), None, "and it is gone");
        assert!(p.leave(a).is_some());
        assert!(p.all_but(ClientId::nil()).is_empty());
    }
}
