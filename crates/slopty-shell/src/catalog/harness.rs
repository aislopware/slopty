// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! slopty-harness — the agent drivers (Claude Code, Codex, …) behind one `Harness` trait.
//!
//! The shell reads only the documented manual install command per agent.
//! The drivers themselves are Slopty's workers'.

/// How each agent installs by hand.
pub mod install {
    use slopty_view_proto::HarnessId;

    /// Portable documented fallback for viewers of a device without installer prerequisites.
    pub fn manual_command(id: HarnessId) -> Option<&'static str> {
        Some(match id {
            HarnessId::CLAUDE_CODE => "curl -fsSL https://claude.ai/install.sh | bash",
            HarnessId::CODEX => "npm install -g @openai/codex",
            HarnessId::CURSOR => "curl https://cursor.com/install -fsS | bash",
            HarnessId::OPENCODE => "npm install -g @opencode/cli",
            HarnessId::PI => "npm install -g --ignore-scripts @earendil-works/pi-coding-agent",
            HarnessId::GROK => "npm install -g @xai-official/grok",
            HarnessId::HERMES => {
                "curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash"
            }
            HarnessId::DEVIN => "curl -fsSL https://cli.devin.ai/install.sh | bash",
            // Any other agent is installed as its own docs say.
            _ => return None,
        })
    }
}
