//! Golden byte snapshots of what a machine says of itself and what the person answers through
//! an agent's published doors: the greeting with its settings file and the agents it can start,
//! and an allow whose call the person changed first. A changed snapshot is a wire change: accept
//! it deliberately (`cargo insta review`).

#[cfg(test)]
mod golden_machine {
    use slopty_core::{SessionId, WallMs, WorkerId};
    use slopty_proto::conversation::{
        ConversationRequest, EditDetail, Patch, PermissionPrompt, ToolDetail, Verdict,
    };
    use slopty_proto::handshake::HelloAck;
    use slopty_proto::server::{InstalledAgent, Os, WorkerCaps};
    use slopty_proto::thread::{AgentId, Editable};
    use slopty_proto::{ClientMsg, WorkerMsg, codec};
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

    fn session() -> SessionId {
        SessionId::from_uuid(Uuid::from_u128(0x5e55))
    }

    fn agent(name: AgentId, version: &str) -> InstalledAgent {
        InstalledAgent { agent: name, version: version.to_owned() }
    }

    /// A machine's greeting names its settings file, so a client edits that machine's settings
    /// in a file tile, and every agent it can start a thread of by the name the thread carries:
    /// Claude Code, Codex, pi and an agent reached over ACP alike.
    #[test]
    fn greeting() {
        let caps = WorkerCaps {
            os: Os::Linux,
            os_version: "Ubuntu 26.04".to_owned(),
            arch: "x86_64".to_owned(),
            cpus: 16,
            memory: 64 << 30,
            encoders: Vec::new(),
            displays: Vec::new(),
            agents: vec![
                agent(AgentId::named(AgentId::CLAUDE_CODE), "2.1.286 (Claude Code)"),
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

    /// An edit's approval offers its new text for the person to change, and the allow that
    /// carries what they left goes to Claude Code as the call's changed input.
    #[test]
    fn edited_allow() {
        let prompt = PermissionPrompt {
            session: session(),
            ask: 7,
            tool: "Edit".to_owned(),
            detail: ToolDetail::Edit(EditDetail {
                path: "/w/a.rs".to_owned(),
                edits: 1,
                replace_all: false,
                patch: Patch {
                    hunks: Vec::new(),
                    added: 1,
                    removed: 1,
                    clipped_lines: 0,
                    full: None,
                },
            }),
            suggestions: Vec::new(),
            mode: Some("default".to_owned()),
            editable: vec![Editable {
                field: "new_string".to_owned(),
                text: "fn b() {}".to_owned(),
            }],
            asked_ms: WallMs::from_millis(1_790_000_000_000),
            until_ms: WallMs::from_millis(1_790_000_595_000),
        };
        snap("machine_permission_editable", &prompt);
        let verdict =
            Verdict::AllowEdited { input: r#"{"new_string":"fn b() -> u8 { 1 }"}"#.to_owned() };
        snap(
            "machine_answer_edited",
            &ClientMsg::Conversation(ConversationRequest::Answer {
                session: session(),
                ask: 7,
                verdict,
            }),
        );
    }
}
