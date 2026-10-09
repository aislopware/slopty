//! How a repository's run script runs in its terminal (`docs/decisions/projects.md`, "The
//! repository's run and archive scripts").
//!
//! Through the person's login shell, interactive, as their own terminal runs a command, so
//! their `PATH`, toolchains and aliases apply. When it ends, however it ends (Ctrl-C on a dev
//! server included), the terminal becomes that shell, in the same folder: the person goes on
//! from there, and it is theirs to close.

/// The command line a script's terminal runs, with `shell` the person's login shell.
#[must_use]
pub fn command_line(line: &str, shell: &str) -> Vec<String> {
    let then = format!("{line}\nexec {} -l", slopty_core::shell_quote(shell));
    [shell, "-l", "-i", "-c", &then].map(str::to_owned).to_vec()
}

/// The person's login shell, as the worker's environment names it; `/bin/sh` when it does not.
#[must_use]
pub fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The script runs in the login shell, interactive, and that shell takes the terminal over
    /// once it ends; a shell path with a space in it is quoted.
    #[test]
    fn a_script_runs_in_the_login_shell_and_leaves_it() {
        assert_eq!(
            command_line("bun run dev", "/bin/zsh"),
            ["/bin/zsh", "-l", "-i", "-c", "bun run dev\nexec /bin/zsh -l"]
        );
        let spaced = command_line("make", "/opt/my shells/fish");
        assert_eq!(spaced.last().map(String::as_str), Some("make\nexec '/opt/my shells/fish' -l"));
    }
}
