//! Golden byte snapshots of the person's snoozes (`slopty_proto::snooze`): set and ended
//! through the server, and the list it sends every client. A changed snapshot is a wire
//! change: accept it deliberately (`cargo insta review`).

#[cfg(test)]
mod golden_snooze {
    use slopty_core::{SessionId, WallMs, WorkerId};
    use slopty_proto::codec;
    use slopty_proto::orchestration::{Outcome, TermRef, Verb};
    use slopty_proto::server::{FromServer, ToServer};
    use slopty_proto::snooze::{Snooze, SnoozeOf, Until};
    use slopty_proto::thread::ThreadId;
    use slopty_proto::thread::attention::ThreadAt;
    use uuid::Uuid;

    fn hex(bytes: &[u8]) -> String {
        bytes
            .chunks(16)
            .map(|row| row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[track_caller]
    fn snap<T: serde::Serialize>(name: &str, msg: &T) {
        let bytes = codec::encode(msg).expect("encodes");
        insta::assert_snapshot!(name, hex(&bytes));
    }

    fn worker() -> WorkerId {
        WorkerId::from_uuid(Uuid::from_u128(0x0199_a000_0000_7000_8000_0000_0000_0001))
    }

    fn thread() -> SnoozeOf {
        let thread =
            ThreadId::from_uuid(Uuid::from_u128(0x0199_a1b1_c3d4_7000_8000_0000_0000_7e7e));
        SnoozeOf::Thread(ThreadAt { worker: worker(), thread })
    }

    fn tile() -> SnoozeOf {
        let session =
            SessionId::from_uuid(Uuid::from_u128(0x0199_a1b1_c3d4_7000_8000_0000_0000_abcd));
        SnoozeOf::Tile(TermRef { worker: worker(), session })
    }

    fn snooze() -> Snooze {
        Snooze {
            of: thread(),
            until_ms: WallMs::from_millis(1_790_003_600_000),
            since_ms: WallMs::from_millis(1_790_000_000_000),
        }
    }

    fn request(verb: Verb) -> ToServer {
        ToServer::Request { id: 21, key: None, verb }
    }

    #[test]
    fn snooze_verbs() {
        let evening = Verb::Snooze {
            of: thread(),
            until: Until::ThisEvening,
            zone: Some("Europe/Berlin".to_owned()),
        };
        snap("verb_snooze_this_evening", &request(evening));
        let at = Until::At(WallMs::from_millis(1_790_086_400_000));
        snap("verb_snooze_at", &request(Verb::Snooze { of: tile(), until: at, zone: None }));
        snap("verb_unsnooze", &request(Verb::Unsnooze { of: tile() }));
        let reply = FromServer::Reply { id: 21, outcome: Outcome::Snoozed(snooze()) };
        snap("outcome_snoozed", &reply);
    }

    #[test]
    fn snoozes_to_every_client() {
        let on_tile = Snooze { of: tile(), ..snooze() };
        snap("from_server_snoozes", &FromServer::Snoozes(vec![snooze(), on_tile]));
    }
}
