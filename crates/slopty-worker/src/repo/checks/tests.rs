use std::os::unix::fs::PermissionsExt as _;

use super::*;

const AT: WallMs = WallMs::from_millis(1_790_000_000_000);

/// What `gh pr checks --json name,bucket` prints, counted: a failure or a cancel fails them
/// all and is named, a check still running holds them pending, a skip passes, and a bucket gh
/// adds later is not taken for a pass.
#[test]
fn gh_s_checks_are_counted_and_judged_together() {
    let json = r#"[
        {"name":"lint","bucket":"pass"},
        {"name":"test (linux)","bucket":"fail"},
        {"name":"deploy","bucket":"skipping"},
        {"name":"bench","bucket":"cancel"},
        {"name":"docs","bucket":"pending"}
    ]"#;
    let checks = gh(json, AT).expect("gh's JSON");
    assert_eq!(checks.state, ChecksState::Failing);
    assert_eq!((checks.passed, checks.failed, checks.pending, checks.skipped), (1, 2, 1, 1));
    assert_eq!(checks.failing, ["test (linux)", "bench"]);

    let running = gh(r#"[{"name":"a","bucket":"pass"},{"name":"b","bucket":"queued"}]"#, AT);
    assert_eq!(running.map(|c| c.state), Ok(ChecksState::Pending), "an unknown bucket waits");
    let passing = gh(r#"[{"name":"a","bucket":"pass"},{"name":"b","bucket":"skipping"}]"#, AT);
    assert_eq!(passing.map(|c| c.state), Ok(ChecksState::Passing));
    assert_eq!(gh("[]", AT).map(|c| c.state), Ok(ChecksState::None));
    assert!(matches!(gh("not json", AT), Err(Failed::Said(_))));

    let many: Vec<String> = (0..9)
        .map(|n| format!(r#"{{"name":"{}","bucket":"fail"}}"#, "x".repeat(200 + n)))
        .collect();
    let many = gh(&format!("[{}]", many.join(",")), AT).expect("gh's JSON");
    assert_eq!((many.failed, many.failing.len()), (9, CHECKS_NAMED));
    assert!(many.failing.iter().all(|n| n.len() == CHECK_NAME_MAX));
}

/// A merge request's pipeline, as `glab mr view --output json` prints it, is one check.
#[test]
fn a_merge_request_s_pipeline_is_one_check() {
    let failed = glab(r#"{"iid":4,"head_pipeline":{"id":9,"status":"failed"}}"#, AT);
    let failed = failed.expect("glab's JSON");
    assert_eq!(
        (failed.state, failed.failing.as_slice()),
        (ChecksState::Failing, &["pipeline".to_owned()][..])
    );
    let running = glab(r#"{"head_pipeline":{"status":"running"}}"#, AT).map(|c| c.state);
    assert_eq!(running, Ok(ChecksState::Pending));
    let none = glab(r#"{"iid":4,"head_pipeline":null}"#, AT).map(|c| c.state);
    assert_eq!(none, Ok(ChecksState::None));
}

/// A stand-in `name` in `dir` that prints `stdout`, says `stderr` and ends with `code`.
fn stand_in(dir: &Path, name: &str, stdout: &str, stderr: &str, code: i32) {
    let script =
        format!("#!/bin/sh\nprintf '%s' '{stdout}'\nprintf '%s' '{stderr}' >&2\nexit {code}\n");
    let path = dir.join(name);
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The forge's own command runs in the checkout: its JSON is read whatever its exit says (gh
/// ends 8 while checks run), "no checks reported" is no checks, and a command not signed in
/// says why. A worker without it says so.
#[tokio::test]
async fn the_forge_s_command_is_run_in_the_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.clone().into_os_string();
    let read = |mr| read_on(Some(&path), dir.path(), 7, mr);

    stand_in(&bin, "gh", r#"[{"name":"ci","bucket":"pending"}]"#, "", 8);
    assert_eq!(read(false).await.map(|c| c.state), Ok(ChecksState::Pending));
    stand_in(&bin, "gh", "", "no checks reported on the \"work\" branch", 1);
    assert_eq!(read(false).await.map(|c| c.state), Ok(ChecksState::None));
    stand_in(&bin, "gh", "", "To get started with GitHub CLI, please run:  gh auth login", 4);
    assert_eq!(
        read(false).await,
        Err(Failed::Said("To get started with GitHub CLI, please run:  gh auth login".to_owned()))
    );
    if find("glab", None).is_none() {
        assert_eq!(read(true).await, Err(Failed::Missing("glab")));
    }
}
