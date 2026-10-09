//! Every sender of a message asks the thread's meta how it goes, now or after the turn
//! (`ThreadMeta::delivery_now`, `ThreadMeta::delivery_after_turn`). Here each adapter's own
//! capabilities meet both rules: the delivery they give is one the worker lets through to that
//! adapter, by the check its intent path makes first (the intent's `needs`, which the thread's
//! meta must `can`).

#[cfg(test)]
mod tests {
    use slopty_agent::driven::caps;
    use slopty_agent::pi::driven::Driven;
    use slopty_agent::{acp, codex, observed, pi};
    use slopty_core::WallMs;
    use slopty_proto::thread::wire::Intent;
    use slopty_proto::thread::{Delivery, ThreadMeta};

    /// A thread's meta with `names` for its capabilities.
    fn meta_with(names: &[&str]) -> ThreadMeta {
        let (driven, _) =
            Driven::new("00000000-0000-7000-8000-000000000001", "1.0.0", "/w", WallMs::ZERO);
        ThreadMeta { caps: caps(names), ..driven.meta().clone() }
    }

    /// Whether the worker lets `delivery` through to a thread of `meta`.
    fn taken(meta: &ThreadMeta, delivery: Delivery) -> bool {
        let send = Intent::Send { text: "go on".to_owned(), delivery, attachments: Vec::new() };
        send.needs().is_none_or(|needs| meta.can(needs))
    }

    #[test]
    fn every_adapter_takes_a_message_sent_now_or_after_the_turn() {
        let adapters: [(&str, &[&str]); 4] = [
            ("Claude Code", &observed::CAPS),
            ("Codex", &codex::shared::CAPS),
            ("pi", &pi::driven::CAPS),
            ("ACP", &acp::driven::CAPS),
        ];
        for (agent, names) in adapters {
            let meta = meta_with(names);
            for (rule, delivery) in
                [("now", meta.delivery_now()), ("after the turn", meta.delivery_after_turn())]
            {
                assert!(taken(&meta, delivery), "{agent}: a message {rule} goes as {delivery:?}");
            }
        }
        // ACP takes no message mid-turn: one sent now waits for the turn's end rather than steer.
        assert_eq!(meta_with(&acp::driven::CAPS).delivery_now(), Delivery::Queue);
    }
}
