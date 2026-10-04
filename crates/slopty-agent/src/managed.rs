//! Claude Code under an organization's management: the managed settings that decide what Slopty
//! may add to a run, and the managed launcher some organizations put in `claude`'s place.
//!
//! **Managed settings** rank above everything else Claude Code reads, and some of their keys
//! make it refuse a run Slopty wired: with `disableSideloadFlags` it exits on `--plugin-dir` or
//! `--mcp-config`. So [`ManagedSettings::read`] reads the system file and the server-delivered
//! cache before every wiring, and [`crate::hooks::wired`], [`crate::hooks::with_mcp`] and
//! [`crate::claude_mod::Installed::args`] leave out what the policy forbids or would ignore.
//! Only the keys listed on [`ManagedSettings`] are read; of the `env` block only the two names
//! that silence Slopty, and only whether they are set. Nothing else of the files is kept.
//!
//! **A managed launcher** answers `claude`, checks in with its control plane on every run, and
//! starts the real client as its child (`docs/decisions/claude-code.md`, "A managed `claude`").
//! Running it in the background costs a control-plane round trip and on a machine not enrolled
//! opens a browser, so the worker never asks it `--version` or `agents`: it asks once whether it
//! is one ([`MANAGED_HELP`], which such a launcher answers before any I/O), takes the version
//! from the client's path ([`artifact_version`]) or its artifacts ([`newest_artifact`]), and the
//! live sessions from Claude Code's own registry ([`crate::roster::registered`]).

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The flag a managed launcher answers with its own usage, before any I/O.
pub const MANAGED_HELP: &str = "--managed-help";

/// The directory, under the person's data directory, that holds a managed launcher's clients:
/// `claude-managed/artifacts/<version>/<platform>/claude`.
const ARTIFACTS: [&str; 2] = ["claude-managed", "artifacts"];

/// The system-wide managed settings file, where Claude Code reads it.
#[cfg(target_os = "macos")]
const SYSTEM_FILE: &str = "/Library/Application Support/ClaudeCode/managed-settings.json";

/// The system-wide managed settings file, where Claude Code reads it.
#[cfg(not(target_os = "macos"))]
const SYSTEM_FILE: &str = "/etc/claude-code/managed-settings.json";

/// The variable that blocks a plugin's requests, the mod's own included.
const NONESSENTIAL_TRAFFIC: &str = "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC";

/// The variable that stops Claude Code titling its terminal.
const TERMINAL_TITLE: &str = "CLAUDE_CODE_DISABLE_TERMINAL_TITLE";

/// What a surface of Claude Code `strictPluginOnlyCustomization` may confine to plugins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PluginOnly {
    /// Nothing is confined.
    #[default]
    None,
    /// Every surface.
    All,
    /// These surfaces (`hooks`, `mcp`, …).
    Some(Vec<String>),
}

impl PluginOnly {
    /// Whether `surface` takes only what a plugin brings.
    #[must_use]
    pub fn confines(&self, surface: &str) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Some(surfaces) => surfaces.iter().any(|s| s == surface),
        }
    }
}

/// An MCP server as a managed allow or deny list names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpMatch {
    /// By its name (`serverName`).
    Name(String),
    /// By its command line (`serverCommand`).
    Command(Vec<String>),
    /// By its URL (`serverUrl`), which no stdio server matches.
    Url(String),
}

impl McpMatch {
    /// Whether the stdio server `name`, run as `command`, is this one.
    fn matches(&self, name: &str, command: &[&str]) -> bool {
        match self {
            Self::Name(n) => n == name,
            Self::Command(c) => c.iter().map(String::as_str).eq(command.iter().copied()),
            Self::Url(_) => false,
        }
    }
}

/// What the organization's managed settings say of the keys that touch Slopty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManagedSettings {
    /// `disableSideloadFlags`: Claude Code refuses to start with `--plugin-dir` or
    /// `--mcp-config`.
    pub disable_sideload_flags: bool,
    /// `disableAllHooks`.
    pub disable_all_hooks: bool,
    /// `allowManagedHooksOnly`: only hooks the managed settings name run.
    pub allow_managed_hooks_only: bool,
    /// `strictPluginOnlyCustomization`.
    pub plugin_only: PluginOnly,
    /// `allowManagedMcpServersOnly`: only the managed allow list admits a server.
    pub allow_managed_mcp_servers_only: bool,
    /// `allowedMcpServers`; `None` when no list is given.
    pub allowed_mcp_servers: Option<Vec<McpMatch>>,
    /// `deniedMcpServers`.
    pub denied_mcp_servers: Vec<McpMatch>,
    /// `disableAgentView`: `claude agents` is off.
    pub disable_agent_view: bool,
    /// `availableModels`; `None` when no list is given.
    pub available_models: Option<Vec<String>>,
    /// `enforceAvailableModels`.
    pub enforce_available_models: bool,
    /// `permissions.defaultMode`: the mode a session starts in unless told another.
    pub default_mode: Option<String>,
    /// `env.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` is set, which blocks the mod's requests.
    pub disables_nonessential_traffic: bool,
    /// `env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE` is set, which stops the title's heartbeat.
    pub disables_terminal_title: bool,
}

impl ManagedSettings {
    /// What this machine's managed settings say now: the system file, then the cache of what
    /// the organization's server delivered (`~/.claude/remote-settings.json` under `home`).
    /// A file that is missing or does not parse says nothing.
    #[must_use]
    pub fn read(home: &Path) -> Self {
        Self::from_files(&[
            PathBuf::from(SYSTEM_FILE),
            home.join(".claude").join("remote-settings.json"),
        ])
    }

    /// [`Self::read`] for this user.
    #[must_use]
    pub fn current() -> Self {
        Self::read(&slopty_platform::dirs::home())
    }

    /// What `files` say together: a switch any of them turns on is on, and the lists are joined.
    #[must_use]
    pub fn from_files(files: &[PathBuf]) -> Self {
        let mut settings = Self::default();
        for file in files {
            let Ok(text) = std::fs::read_to_string(file) else { continue };
            let Ok(doc) = serde_json::from_str::<Value>(&text) else { continue };
            settings.add(&doc);
        }
        settings
    }

    /// Take in one settings document's listed keys.
    pub fn add(&mut self, doc: &Value) {
        let flag = |key: &str| doc.get(key).and_then(Value::as_bool).unwrap_or(false);
        self.disable_sideload_flags |= flag("disableSideloadFlags");
        self.disable_all_hooks |= flag("disableAllHooks");
        self.allow_managed_hooks_only |= flag("allowManagedHooksOnly");
        self.allow_managed_mcp_servers_only |= flag("allowManagedMcpServersOnly");
        self.disable_agent_view |= flag("disableAgentView");
        self.enforce_available_models |= flag("enforceAvailableModels");
        self.plugin_only = match (doc.get("strictPluginOnlyCustomization"), &self.plugin_only) {
            (_, PluginOnly::All) | (Some(Value::Bool(true)), _) => PluginOnly::All,
            (Some(Value::Array(surfaces)), was) => {
                let mut all = match was {
                    PluginOnly::Some(had) => had.clone(),
                    _ => Vec::new(),
                };
                all.extend(surfaces.iter().filter_map(Value::as_str).map(str::to_owned));
                PluginOnly::Some(all)
            }
            (_, was) => was.clone(),
        };
        if let Some(Value::Array(list)) = doc.get("allowedMcpServers") {
            self.allowed_mcp_servers.get_or_insert_with(Vec::new).extend(mcp_matches(list));
        }
        if let Some(Value::Array(list)) = doc.get("deniedMcpServers") {
            self.denied_mcp_servers.extend(mcp_matches(list));
        }
        if let Some(Value::Array(models)) = doc.get("availableModels") {
            let names = models.iter().filter_map(Value::as_str).map(str::to_owned);
            self.available_models.get_or_insert_with(Vec::new).extend(names);
        }
        if let Some(mode) = doc.pointer("/permissions/defaultMode").and_then(Value::as_str) {
            self.default_mode.get_or_insert_with(|| mode.to_owned());
        }
        let set = |name: &str| {
            doc.get("env").and_then(|env| env.get(name)).is_some_and(|value| {
                value.as_str().is_some_and(|v| !v.is_empty()) || value.is_number()
            })
        };
        self.disables_nonessential_traffic |= set(NONESSENTIAL_TRAFFIC);
        self.disables_terminal_title |= set(TERMINAL_TITLE);
    }

    /// Whether a run may carry `--plugin-dir` and `--mcp-config`: Claude Code refuses to start
    /// with either while sideloading is off.
    #[must_use]
    pub const fn sideloads(&self) -> bool {
        !self.disable_sideload_flags
    }

    /// Whether hooks Slopty registers on a run (`--settings`) run: not while every hook is off,
    /// only managed ones run, or hooks come only from plugins.
    #[must_use]
    pub fn runs_own_hooks(&self) -> bool {
        !self.disable_all_hooks
            && !self.allow_managed_hooks_only
            && !self.plugin_only.confines("hooks")
    }

    /// Whether the mod can load and be heard: a plugin directory is allowed, its function hooks
    /// load (they are hooks, under the same switches, though a plugin's), and its requests are
    /// not blocked ([`Self::mod_off`]).
    #[must_use]
    pub const fn hears_mod(&self) -> bool {
        self.mod_off(None).is_none()
    }

    /// Why the mod could not be heard in a run of Claude Code under these settings whose own
    /// environment holds `inherited` as `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` (`None` for
    /// an agent the worker starts, which sets it empty); `None` when it can be.
    ///
    /// Claude Code's plugin network gate refuses a sideloaded plugin's every request while that
    /// variable is set to anything but empty, so a mod loaded under it never says hello.
    /// Slopty clears it only for the agents it starts; a `claude` the person typed keeps
    /// theirs, and the mod is left out of it.
    #[must_use]
    pub const fn mod_off(&self, inherited: Option<&str>) -> Option<ModOff> {
        if self.disable_sideload_flags {
            Some(ModOff::SideloadingOff)
        } else if self.disable_all_hooks {
            Some(ModOff::HooksOff)
        } else if self.allow_managed_hooks_only {
            Some(ModOff::ManagedHooksOnly)
        } else if self.disables_nonessential_traffic {
            Some(ModOff::QuietByPolicy)
        } else if let Some(value) = inherited
            && !value.is_empty()
        {
            Some(ModOff::QuietByEnvironment)
        } else {
            None
        }
    }

    /// Whether the stdio MCP server `name`, run as `command`, may serve a run.
    #[must_use]
    pub fn admits_mcp(&self, name: &str, command: &[&str]) -> bool {
        if !self.sideloads() || self.plugin_only.confines("mcp") {
            return false;
        }
        if self.denied_mcp_servers.iter().any(|m| m.matches(name, command)) {
            return false;
        }
        match &self.allowed_mcp_servers {
            Some(allowed) => allowed.iter().any(|m| m.matches(name, command)),
            None => !self.allow_managed_mcp_servers_only,
        }
    }
}

/// The servers a managed list names.
fn mcp_matches(list: &[Value]) -> impl Iterator<Item = McpMatch> + '_ {
    list.iter().filter_map(|entry| {
        if let Some(name) = entry.get("serverName").and_then(Value::as_str) {
            return Some(McpMatch::Name(name.to_owned()));
        }
        if let Some(Value::Array(words)) = entry.get("serverCommand") {
            let words = words.iter().filter_map(Value::as_str).map(str::to_owned).collect();
            return Some(McpMatch::Command(words));
        }
        entry.get("serverUrl").and_then(Value::as_str).map(|url| McpMatch::Url(url.to_owned()))
    })
}

/// Why Slopty's Claude Code mod is left out of a run, where it could not be heard: the thread
/// then follows the hooks, the transcript and the status line alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModOff {
    /// `disableSideloadFlags`: Claude Code would refuse the run for `--plugin-dir`.
    SideloadingOff,
    /// `disableAllHooks`: a plugin's function hooks are hooks.
    HooksOff,
    /// `allowManagedHooksOnly`: only the hooks the managed settings name run.
    ManagedHooksOnly,
    /// The managed settings set `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`, which blocks a
    /// plugin's requests.
    QuietByPolicy,
    /// The run's own environment sets `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`.
    QuietByEnvironment,
}

impl std::fmt::Display for ModOff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SideloadingOff => "the managed settings turn sideloading off (disableSideloadFlags)",
            Self::HooksOff => "the managed settings turn hooks off (disableAllHooks)",
            Self::ManagedHooksOnly => {
                "the managed settings run only their own hooks (allowManagedHooksOnly)"
            }
            Self::QuietByPolicy => {
                "the managed settings set CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC, which blocks a plugin's requests"
            }
            Self::QuietByEnvironment => {
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC is set here, which blocks a plugin's requests"
            }
        })
    }
}

/// What `claude` is on a machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Launcher {
    /// Claude Code itself, or nothing that says otherwise.
    #[default]
    Plain,
    /// A managed launcher that starts Claude Code as its child.
    Managed,
}

impl Launcher {
    /// What `claude` is, from how it answered [`MANAGED_HELP`]: a managed launcher prints its
    /// usage and succeeds; Claude Code refuses a flag it does not know.
    #[must_use]
    pub fn from_managed_help(succeeded: bool, out: &str) -> Self {
        if succeeded && out.contains("managed") { Self::Managed } else { Self::Plain }
    }

    /// Whether `claude` must not be run in the background (`--version`, `agents`).
    #[must_use]
    pub const fn is_managed(self) -> bool {
        matches!(self, Self::Managed)
    }
}

/// The Claude Code version a managed launcher's client at `exe` is: the `<version>` of
/// `…/claude-managed/artifacts/<version>/<platform>/claude`.
#[must_use]
pub fn artifact_version(exe: &Path) -> Option<String> {
    let parts: Vec<&str> = exe.iter().filter_map(|p| p.to_str()).collect();
    let at = parts.windows(2).position(|w| w == ARTIFACTS)?;
    let version = parts.get(at.checked_add(2)?)?;
    // The platform and the program follow the version.
    parts.get(at.checked_add(4)?)?;
    is_version(version).then(|| (*version).to_owned())
}

/// Whether `exe` is a managed launcher's client.
#[must_use]
pub fn is_artifact(exe: &Path) -> bool {
    artifact_version(exe).is_some()
}

/// The newest Claude Code a managed launcher has under `data_dir` (`~/.local/share`), by its
/// artifacts' directory names: the version its next run starts, read without running it.
#[must_use]
pub fn newest_artifact(data_dir: &Path) -> Option<String> {
    let dir = ARTIFACTS.iter().fold(data_dir.to_path_buf(), |dir, part| dir.join(part));
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| is_version(name))
        .max_by(|a, b| compare_versions(a, b))
}

/// Whether `text` is a dotted version (`2.1.289`).
fn is_version(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// Two dotted versions in order, part by part as numbers.
#[must_use]
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |v: &str| v.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(a).cmp(&parts(b))
}

#[cfg(test)]
mod tests;
