//! Golden byte snapshots of what a machine says of itself: the greeting with its settings file
//! and the agents it can start. A changed snapshot is a wire change: accept it deliberately
//! (`cargo insta review`).

#[cfg(test)]
mod golden_machine {
    use slopty_core::WorkerId;
    use slopty_proto::handshake::HelloAck;
    use slopty_proto::server::{InstalledAgent, Os, WorkerCaps};
    use slopty_proto::thread::{AgentId, Effort, Mode, Model, Offers};
    use slopty_proto::{WorkerMsg, codec};
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

    fn agent(name: AgentId, version: &str) -> InstalledAgent {
        InstalledAgent { agent: name, version: version.to_owned(), offers: Offers::default() }
    }

    /// A machine's greeting names its settings file, so a client edits that machine's settings
    /// in a file tile, and every agent it can start a thread of by the name the thread carries:
    /// Claude Code, Codex, pi and an agent reached over ACP alike, each with what a new thread of
    /// it can be started with.
    #[test]
    fn greeting() {
        let caps = WorkerCaps {
            os: Os::Linux,
            os_version: "Ubuntu 26.04".to_owned(),
            arch: "x86_64".to_owned(),
            form: slopty_proto::server::Form::Desktop,
            cpus: 16,
            memory: 64 << 30,
            encoders: Vec::new(),
            displays: Vec::new(),
            agents: vec![
                InstalledAgent {
                    offers: Offers {
                        models: vec![Model { id: "opus".to_owned(), label: "Opus".to_owned() }],
                        modes: vec![Mode {
                            id: "plan".to_owned(),
                            label: "Plan".to_owned(),
                            description: Some("Plans first and changes nothing".to_owned()),
                        }],
                        efforts: vec![Effort {
                            id: "high".to_owned(),
                            label: "High".to_owned(),
                            description: None,
                        }],
                        commands: Vec::new(),
                    },
                    ..agent(AgentId::named(AgentId::CLAUDE_CODE), "2.1.286 (Claude Code)")
                },
                agent(AgentId::named(AgentId::CODEX), "codex-cli 0.157.0"),
                agent(AgentId::named(AgentId::PI), "0.42.1"),
                agent(AgentId::acp("gemini"), "0.9.0"),
            ],
            can_capture: false,
            can_inject: false,
            virtual_displays: false,
            version: "0.1.0".to_owned(),
            lan: Vec::new(),
            wake_on_lan: None,
            writes_failing: None,
            stops_at_logout: None,
        };
        snap(
            "machine_hello_ack",
            &WorkerMsg::HelloAck(HelloAck {
                worker: WorkerId::from_uuid(Uuid::from_u128(0x77)),
                name: "devbox".to_owned(),
                home: "/home/w".to_owned(),
                settings: "/home/w/.local/share/slopty/settings.toml".to_owned(),
                caps,
                load: 0.5,
                sessions: Vec::new(),
            }),
        );
    }
}
