//! `xtask warm uninstall`: everything `install`, `register` and `try` put on this Mac and on
//! GitHub taken away, and the warm runners' volume detached.
//!
//! As the person: both runners unregistered from GitHub, `SLOPTY_RUNNERS` dropped when it sends
//! the gate to them (so the gate runs on GitHub's runners again), `try`'s agent and the VM
//! stopped, and the runner's registration taken off the volume. Then, when the administrator's
//! part was ever run, the script that undoes it ([`super::mac::uninstall_script`]), through
//! `sudo`: the daemons, the log directory, the runner's account and its home, and the volume
//! detached. Last, the volume detached unless it is, and its image deleted when asked.

use anyhow::{Context as _, Result};
use camino::Utf8Path;
use xshell::{Shell, cmd};

use super::{LINUX_LABEL, MAC_LABEL, mac};

/// The repository variable that sends the gate to the warm runners when it is `warm`.
const ROUTE: &str = "SLOPTY_RUNNERS";

/// `xtask warm uninstall`: the warm runners at `home`, their volume's `image` deleted too when
/// `delete_image` says so, and the administrator's part written to `script`; with `dry_run`,
/// what it would do, printed, and nothing done but writing that script.
pub fn run(
    sh: &Shell,
    home: &Utf8Path,
    image: &Utf8Path,
    script: &Utf8Path,
    delete_image: bool,
    dry_run: bool,
) -> Result<()> {
    let act = |what: &str, step: &mut dyn FnMut() -> Result<()>| -> Result<()> {
        if dry_run {
            println!("  would {what}");
            return Ok(());
        }
        step().with_context(|| what.to_owned())?;
        println!("  ✓ {what}");
        Ok(())
    };
    let repository =
        cmd!(sh, "gh repo view --json nameWithOwner --jq .nameWithOwner").quiet().read()?;
    println!("▶ the warm runners on GitHub ({repository})");
    for name in [MAC_LABEL, LINUX_LABEL] {
        let path = format!("repos/{repository}/actions/runners?per_page=100");
        let query = format!(".runners[] | select(.name == \"{name}\") | .id");
        let ids = cmd!(sh, "gh api {path} --jq {query}").quiet().read()?;
        for id in ids.lines().filter(|id| !id.is_empty()) {
            let runner = format!("repos/{repository}/actions/runners/{id}");
            act(&format!("unregister {name} ({id})"), &mut || {
                cmd!(sh, "gh api -X DELETE {runner}").quiet().ignore_stdout().run()?;
                Ok(())
            })?;
        }
    }
    let variable = format!("repos/{repository}/actions/variables/{ROUTE}");
    let route = cmd!(sh, "gh api {variable} --jq .value")
        .quiet()
        .ignore_status()
        .ignore_stderr()
        .read()
        .unwrap_or_default();
    if route.trim() == "warm" {
        act(&format!("drop {ROUTE}, so that the gate runs on GitHub's runners"), &mut || {
            cmd!(sh, "gh api -X DELETE {variable}").quiet().run()?;
            Ok(())
        })?;
    }
    println!("▶ this Mac");
    act("stop try's agent and remove its files", &mut || mac::remove_try(sh))?;
    if super::mounted(home) {
        act("stop the VM, and the supervisor `warm linux start` started", &mut || {
            // The daemon's supervisor is the administrator's part's to stop.
            if let Err(error) = super::linux::stop(sh, home) {
                println!("  the VM: {error:#}");
            }
            Ok(())
        })?;
        act("take the Mac runner's registration off the volume", &mut || {
            mac::forget_registration(home)
        })?;
    }
    if let Some(dir) = script.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {dir}"))?;
    }
    mac::write(script, &mac::uninstall_script(script))?;
    if mac::administered() {
        act(&format!("undo the administrator's part: sudo sh {script}"), &mut || {
            cmd!(sh, "sudo sh {script}").run()?;
            Ok(())
        })?;
    } else {
        println!("  the administrator's part was never run; its undoing is in {script}");
    }
    if super::mounted(home) {
        act(&format!("detach {home}"), &mut || {
            cmd!(sh, "hdiutil detach {home}").quiet().ignore_stdout().run()?;
            Ok(())
        })?;
    }
    if delete_image && image.exists() {
        act(&format!("delete {image}, and everything the runners kept in it"), &mut || {
            anyhow::ensure!(!super::mounted(home), "{home} is still attached");
            std::fs::remove_dir_all(image).with_context(|| format!("delete {image}"))
        })?;
    } else if image.exists() {
        println!("  {image} stays, with the runners' toolchains and builds: `--image` deletes it");
    }
    Ok(())
}
