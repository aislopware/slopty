use std::sync::Arc;

use slopty_core::SessionId;
use tokio::sync::oneshot::error::TryRecvError;

use super::*;

/// One turn at a time, and when it ends the largest session still waiting goes next; a
/// session that stopped waiting is passed over, and one that asks again is asked once, at its
/// new size.
#[test]
fn turns_go_one_at_a_time_to_the_largest_first() {
    let [first_id, small_id, large_id, gone_id, medium_id] =
        std::array::from_fn(|_| SessionId::new());
    let compressor = Arc::new(Compressor::default());
    let first = compressor.ask(first_id, 10).try_recv().expect("the first asker goes at once");

    let mut small = compressor.ask(small_id, 100);
    let mut large = compressor.ask(large_id, 900);
    let gone = compressor.ask(gone_id, 5_000);
    let mut replaced = compressor.ask(medium_id, 1);
    let mut medium = compressor.ask(medium_id, 500);
    assert_eq!(replaced.try_recv().unwrap_err(), TryRecvError::Closed, "asking again replaces");
    drop(gone);
    assert_eq!(small.try_recv().unwrap_err(), TryRecvError::Empty, "one turn at a time");

    drop(first);
    let next = large.try_recv().expect("the largest still waiting goes next");
    assert_eq!(medium.try_recv().unwrap_err(), TryRecvError::Empty);
    drop(next);
    let next = medium.try_recv().expect("then the next largest");
    drop(next);
    let next = small.try_recv().expect("then the smallest");
    drop(next);

    compressor.ask(first_id, 1).try_recv().expect("with no turn out, the next asker goes at once");
}
