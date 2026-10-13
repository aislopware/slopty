// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! HarnessRegistry — the engine's harness catalog: eager instances (mock) plus lazy
//! slots resolved on first use (claude-code spawns subprocess discovery; codex/cursor
//! later). Lazy slots carry a static descriptor so `ListHarnesses` never forces a spawn.
//!
//! Also owns the device's harness ENABLEMENT (Settings → Providers): which harnesses
//! this device's composer offers, persisted in `{data_dir}/harness-prefs.json`.
//! Per-device because CLI installs are — a viewer retargets the settings page at
//! another device and edits THAT device's set over the forwarded RPCs.

// slopty: an excerpt — the descriptor and title settings the interface reads. The registry
// itself, its harness slots and prefs file are the engine's, which is Slopty's.

use serde::{Deserialize, Serialize};

use slopty_view_proto::{HarnessId, ReasoningLevel, SteeringMode};

/// What `ListHarnesses` reports per harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessDescriptor {
    pub id: HarnessId,
    pub name: String,
    pub supports_steering: bool,
    pub steering_mode: SteeringMode,
    pub reasoning_levels: Vec<ReasoningLevel>,
    /// Whether the agent's CLI is present on the listing device (the settings
    /// enable-gate). Defaults true so catalogs from engines predating the
    /// field never read as uninstallable.
    #[serde(default = "default_installed")]
    pub installed: bool,
    /// Explicit CLI installation is available on this listing device.
    #[serde(default)]
    pub can_install: bool,
    /// Whether the listing device offers this harness (Settings → Providers).
    /// `None` — the catalog came from an engine predating the setting — means
    /// "unknown": consumers fall back to detection (see [`descriptor_enabled`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

impl HarnessDescriptor {
    /// Whether this harness can accept a prompt inside the turn that is
    /// currently running. Turn-boundary steering is still useful to the
    /// automatic queue drain, but it is not the non-interrupting "Steer"
    /// action exposed on an individual queued row.
    pub fn steers_mid_turn(&self) -> bool {
        self.supports_steering && self.steering_mode == SteeringMode::StepBoundary
    }
}

fn default_installed() -> bool {
    true
}

/// Whether detection alone may switch a harness on. The mock always "resolves"
/// but is a test rig, and counting it would let the last real agent be turned
/// off (the composer needs something it can actually run); the dev rig opts it
/// in through `COMET_HARNESS=mock`, which bypasses enablement in the pickers.
fn auto_enabled(id: HarnessId) -> bool {
    id != HarnessId::Mock
}

/// A descriptor's effective enabled flag. `None` — a catalog from an engine
/// predating the setting — falls back to detection, the same rule new devices
/// start from (see [`HarnessRegistry::enabled_set`]).
pub fn descriptor_enabled(descriptor: &HarnessDescriptor) -> bool {
    descriptor
        .enabled
        .unwrap_or_else(|| descriptor.installed && auto_enabled(descriptor.id))
}

/// Per-device automatic session title preferences.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TitleSettings {
    /// None follows the session harness, using a supported installed fallback.
    pub harness: Option<HarnessId>,
    /// None selects the cheapest model offered by the selected harness.
    pub model: Option<String>,
}
