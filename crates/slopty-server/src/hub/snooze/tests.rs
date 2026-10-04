//! Snoozes as the hub keeps them: presets worked out in the person's zone, the time and the
//! thread's news ending them, and the person's alone.

use slopty_core::{SessionId, WorkerId};
use slopty_proto::orchestration::Verb;
use slopty_proto::thread::Phase;
use slopty_proto::thread::attention::ThreadAt;
use slopty_proto::thread::wire::ThreadRow;
use tokio::sync::mpsc;

use super::super::ladder::tests::{asking, row, snapshot};
use super::super::tests::{registration, summary};
use super::super::{Hub, Speaker};
use super::*;

/// The wall time `text` names in `zone`.
fn at(text: &str, zone: &str) -> WallMs {
    let zoned = text.parse::<jiff::civil::DateTime>().unwrap().in_tz(zone).unwrap();
    WallMs::from_millis(u64::try_from(zoned.timestamp().as_millisecond()).unwrap())
}

/// The presets land where the person's zone says, not the server's: the evening at six that
/// day while it is more than an hour off, the morning at nine the next day across a change of
/// clocks, an hour on; a time picked must be ahead.
#[test]
fn presets_are_worked_out_in_the_person_s_zone() {
    let berlin = crate::project::when::zone("Europe/Berlin").unwrap();
    let morning = at("2026-03-28T10:00", "Europe/Berlin");
    assert_eq!(
        until_ms(Until::ThisEvening, morning, &berlin),
        Ok(at("2026-03-28T18:00", "Europe/Berlin"))
    );
    // Clocks go forward overnight: nine the next morning is 22 hours on, not 23.
    let tomorrow = until_ms(Until::Tomorrow, morning, &berlin).unwrap();
    assert_eq!(tomorrow, at("2026-03-29T09:00", "Europe/Berlin"));
    assert_eq!(tomorrow.millis_since(morning), 22 * 3_600_000);
    let hour = until_ms(Until::InAnHour, morning, &berlin).unwrap();
    assert_eq!(hour.millis_since(morning), 3_600_000);

    let late = at("2026-03-28T17:30", "Europe/Berlin");
    let refused = until_ms(Until::ThisEvening, late, &berlin).unwrap_err();
    assert!(refused.contains("snooze until tomorrow"), "{refused}");
    let tokyo = crate::project::when::zone("Asia/Tokyo").unwrap();
    assert_eq!(until_ms(Until::ThisEvening, morning, &tokyo).unwrap_err(), refused, "18:00 there");

    assert_eq!(until_ms(Until::At(late), morning, &berlin), Ok(late));
    assert!(until_ms(Until::At(morning), late, &berlin).is_err(), "behind now");
}

/// A snooze holds until its time; one kept past it is not taken up again; the soonest is
/// what the delivery loop wakes for; a notice for its thread, or its thread's tile, ends it.
#[test]
fn a_snooze_ends_at_its_time_or_with_its_thread_s_news() {
    let (worker, session) = (WorkerId::new(), SessionId::new());
    let thread = ThreadAt { worker, thread: slopty_proto::thread::ThreadId::new() };
    let tile = TermRef { worker, session };
    let snooze = |of, until| Snooze {
        of,
        until_ms: WallMs::from_millis(until),
        since_ms: WallMs::from_millis(1),
    };
    let kept = vec![snooze(SnoozeOf::Thread(thread), 5_000), snooze(SnoozeOf::Tile(tile), 9_000)];
    let mut snoozes = Snoozes::restore(kept.clone(), WallMs::from_millis(6_000));
    assert_eq!(snoozes.list(), [kept[1]], "one over while the server was down is let go");
    let mut snoozes_all = Snoozes::restore(kept, WallMs::from_millis(1_000));
    assert_eq!(snoozes_all.next(), Some(WallMs::from_millis(5_000)));
    assert!(snoozes_all.expire(WallMs::from_millis(5_000)));
    assert!(!snoozes_all.expire(WallMs::from_millis(5_001)), "nothing more to let go");

    let notice = Notice {
        kind: slopty_proto::thread::attention::NoticeKind::Finished,
        thread,
        tile: Some(session),
        title: "t".to_owned(),
        text: String::new(),
        worked_ms: None,
        via: None,
    };
    assert!(snoozes.news(&notice), "the tile the thread runs in");
    assert_eq!(snoozes.list(), []);
    assert!(!snoozes.news(&notice));
}

/// The person snoozes a finish and every client hears the list; an agent is refused; a thread
/// that needs the person is not snoozed; the thread needing them later ends the snooze, and
/// the person can end one at once.
#[tokio::test]
async fn a_snooze_is_the_person_s_and_ends_when_its_thread_needs_them() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (shell, worker) = (SessionId::new(), WorkerId::new());
    let (tx, _rx) = mpsc::channel(8);
    let lease = hub
        .register(registration(worker, vec![summary(shell)]), [100, 64, 0, 7].into(), tx)
        .unwrap();
    let mut heard = hub.subscribe();
    let rank = |rows: Vec<ThreadRow>| {
        lease.handle(snapshot(rows));
        hub.rank_ladder();
    };
    let done = row(Phase::Idle, 1_000, Some(shell));
    rank(vec![done.clone()]);
    let at = ThreadAt { worker, thread: done.id };
    let verb =
        |of| Verb::Snooze { of, until: Until::InAnHour, zone: Some("Europe/Berlin".to_owned()) };

    let refused = hub.dispatch_as(Speaker::Agent, None, verb(SnoozeOf::Thread(at))).await;
    assert!(matches!(&refused, Outcome::Error { code: ErrorCode::Forbidden, .. }), "{refused:?}");
    let Outcome::Snoozed(snooze) = hub.dispatch(verb(SnoozeOf::Thread(at))).await else {
        panic!("not snoozed")
    };
    let ahead = snooze.until_ms.millis_since(snooze.since_ms);
    assert_eq!(ahead, 3_600_000);
    assert_eq!(hub.snoozes(), [snooze]);
    let told = loop {
        if let FromServer::Snoozes(list) = heard.recv().await.unwrap() {
            break list;
        }
    };
    assert_eq!(told, [snooze]);
    assert_eq!(*hub.snoozes_kept().borrow(), [snooze], "and the store");

    rank(vec![asking(done.clone(), "Run tests?")]);
    assert_eq!(hub.snoozes(), [], "it needs the person: the snooze is over");
    let waiting = hub.dispatch(verb(SnoozeOf::Thread(at))).await;
    assert!(
        matches!(&waiting, Outcome::Error { code: ErrorCode::Conflict, message } if message.contains("waits on you")),
        "{waiting:?}"
    );
    let tile = SnoozeOf::Tile(TermRef { worker, session: shell });
    assert!(matches!(hub.dispatch(verb(tile)).await, Outcome::Error { .. }), "nor its tile");

    rank(vec![done.clone()]);
    assert!(matches!(hub.dispatch(verb(SnoozeOf::Thread(at))).await, Outcome::Snoozed(_)));
    assert_eq!(hub.dispatch(Verb::Unsnooze { of: SnoozeOf::Thread(at) }).await, Outcome::Done);
    assert_eq!(hub.snoozes(), []);

    let soon = WallMs::from_millis(WallMs::now().as_millis() + 50);
    let brief = Verb::Snooze { of: SnoozeOf::Thread(at), until: Until::At(soon), zone: None };
    assert!(matches!(hub.dispatch(brief).await, Outcome::Snoozed(_)));
    let next = hub.deliver_due().expect("the delivery loop wakes for it");
    tokio::time::sleep_until(next).await;
    hub.deliver_due();
    assert_eq!(hub.snoozes(), [], "over at its time");
    assert_eq!(*hub.snoozes_kept().borrow(), [], "and the store hears");
    let bad_zone =
        Verb::Snooze { of: tile, until: Until::Tomorrow, zone: Some("Mars/Base".to_owned()) };
    assert!(matches!(
        hub.dispatch(bad_zone).await,
        Outcome::Error { code: ErrorCode::Invalid, .. }
    ));
}
