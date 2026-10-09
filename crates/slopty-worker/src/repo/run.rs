//! A repository's own run and archive scripts, read from the files its other tools keep
//! (`docs/decisions/projects.md`, "The repository's run and archive scripts").
//!
//! The run scripts are what a tool's Run button starts in a workspace: a dev server, a test
//! watch. The archive script runs in a worktree before it is removed, to stop what its setup or
//! a run left going. They are read from the same files as the setup ([`super::setup::SOURCES`]),
//! from the checkout they would run in. For each kind, the first file that names one wins, and
//! sources are never merged, as with the setup.

use std::path::Path;

use super::setup::{Found, SOURCES, joined};

/// One of the commands a repository keeps to run in its folder.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RunScript {
    /// Its name, as its file names it: a Conductor id, a Codex action's or a t3 script's name;
    /// "Run" for the one script of a file that keeps only one.
    pub name: String,
    /// The shell script it runs.
    pub line: String,
    /// Where it runs, relative to the checkout; the checkout itself when `None`.
    pub dir: Option<String>,
    /// Its file marks it as the one to run when nothing is chosen.
    pub default: bool,
}

/// The run scripts the checkout at `tree` keeps, with the file they are read from.
///
/// Those of the first of [`SOURCES`] that names any, the default first. A file that does not
/// parse is passed over, as one with none.
#[must_use]
pub fn find(tree: &Path) -> Option<(&'static str, Vec<RunScript>)> {
    SOURCES.into_iter().find_map(|from| {
        let text = std::fs::read_to_string(tree.join(from)).ok()?;
        let mut scripts: Vec<RunScript> =
            read(from, &text)?.into_iter().filter(|s| !s.line.trim().is_empty()).collect();
        scripts.sort_by_key(|s| !s.default);
        (!scripts.is_empty()).then_some((from, scripts))
    })
}

/// The run scripts of the checkout at `root`, ready to open
/// ([`slopty_proto::git::GitOp::Scripts`]).
///
/// Each runs through the person's login shell, which takes the terminal over once it ends
/// ([`super::script::command_line`]), in its folder, with the places in its environment as a
/// setup from the same file gets them.
#[must_use]
pub fn scripts(root: &Path) -> slopty_proto::git::RunScripts {
    let Some((from, found)) = find(root) else {
        return slopty_proto::git::RunScripts { from: None, list: Vec::new() };
    };
    let clone = super::worktrees::clone_of(root).map_or_else(|_| root.to_path_buf(), |(c, _)| c);
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let at = super::setup::Places { root: &clone, tree: root, name: &name, base: None };
    let env = super::setup::env(from, &at);
    let shell = super::script::login_shell();
    let list = found
        .into_iter()
        .map(|script| slopty_proto::git::RunScript {
            command: super::script::command_line(&script.line, &shell),
            cwd: folder(root, &script).to_string_lossy().into_owned(),
            env: env.clone(),
            name: script.name,
            line: script.line,
        })
        .collect();
    slopty_proto::git::RunScripts { from: Some(from.to_owned()), list }
}

/// The archive script the checkout at `tree` keeps: the first of [`SOURCES`] whose archive is
/// not empty.
#[must_use]
pub fn archive(tree: &Path) -> Option<Found> {
    SOURCES.into_iter().find_map(|from| {
        let text = std::fs::read_to_string(tree.join(from)).ok()?;
        let script = read_archive(from, &text)?.trim().to_owned();
        (!script.is_empty()).then_some(Found { from, script })
    })
}

/// One script of a file that keeps only one.
fn the_one(line: String, dir: Option<String>) -> Vec<RunScript> {
    vec![RunScript { name: "Run".to_owned(), line, dir, default: true }]
}

/// The string at `key` of the table `value`.
fn str_at<'a>(value: &'a toml::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(toml::Value::as_str)
}

/// The run scripts `text`, read as `from`, names.
fn read(from: &str, text: &str) -> Option<Vec<RunScript>> {
    match from {
        // `[scripts] run = "…"`, or named `[scripts.run.<id>]` with `command`, `args`,
        // `options.cwd`, `default` and `hide`.
        ".conductor/settings.toml" => {
            let doc: toml::Table = toml::from_str(text).ok()?;
            let run = doc.get("scripts")?.get("run")?;
            if let Some(line) = run.as_str() {
                return Some(the_one(line.to_owned(), None));
            }
            let named = run.as_table()?.iter().filter_map(|(id, entry)| {
                let hidden = entry.get("hide").and_then(toml::Value::as_bool) == Some(true);
                let command = str_at(entry, "command").filter(|_| !hidden)?;
                let args = entry.get("args").and_then(toml::Value::as_array);
                let args = args.into_iter().flatten().filter_map(toml::Value::as_str);
                let line = std::iter::once(command.to_owned())
                    .chain(args.map(slopty_core::shell_quote))
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(RunScript {
                    name: id.clone(),
                    line,
                    dir: entry.get("options").and_then(|o| str_at(o, "cwd")).map(str::to_owned),
                    default: entry.get("default").and_then(toml::Value::as_bool) == Some(true),
                })
            });
            Some(named.collect())
        }
        // `{"scripts": {"run": "…"}}`.
        "conductor.json" => {
            let doc: serde_json::Value = serde_json::from_str(text).ok()?;
            let line = doc.get("scripts")?.get("run")?.as_str()?;
            Some(the_one(line.to_owned(), None))
        }
        // `[[actions]]`, each a `name` and a `command`.
        ".codex/environments/environment.toml" => {
            let doc: toml::Table = toml::from_str(text).ok()?;
            let actions = doc.get("actions")?.as_array()?.iter().filter_map(|action| {
                Some(RunScript {
                    name: str_at(action, "name")?.to_owned(),
                    line: str_at(action, "command")?.to_owned(),
                    dir: None,
                    default: false,
                })
            });
            Some(actions.collect())
        }
        // `{"run": ["…", …], "cwd": "…"}`.
        ".superset/config.json" => {
            let doc: serde_json::Value = serde_json::from_str(text).ok()?;
            let dir = doc.get("cwd").and_then(serde_json::Value::as_str).map(str::to_owned);
            Some(the_one(joined(doc.get("run")?)?, dir))
        }
        // The scripts not run as the worktree's setup, each a `name` and a `command`.
        "t3.json" => {
            let doc: serde_json::Value = serde_json::from_str(text).ok()?;
            let text_at = |s: &serde_json::Value, key: &str| {
                s.get(key).and_then(serde_json::Value::as_str).map(str::to_owned)
            };
            let scripts = doc.get("scripts")?.as_array()?.iter().filter_map(|s| {
                let setup = s.get("runOnWorktreeCreate").and_then(serde_json::Value::as_bool);
                (setup != Some(true)).then_some(())?;
                Some(RunScript {
                    name: text_at(s, "name")?,
                    line: text_at(s, "command")?,
                    dir: None,
                    default: false,
                })
            });
            Some(scripts.collect())
        }
        _ => None,
    }
}

/// The archive script `text`, read as `from`, names.
fn read_archive(from: &str, text: &str) -> Option<String> {
    match from {
        // `[scripts] archive = "…"`.
        ".conductor/settings.toml" => {
            let doc: toml::Table = toml::from_str(text).ok()?;
            Some(doc.get("scripts")?.get("archive")?.as_str()?.to_owned())
        }
        // `{"scripts": {"archive": "…"}}`.
        "conductor.json" => {
            let doc: serde_json::Value = serde_json::from_str(text).ok()?;
            Some(doc.get("scripts")?.get("archive")?.as_str()?.to_owned())
        }
        // `{"teardown": ["…", …]}`.
        ".superset/config.json" => {
            let doc: serde_json::Value = serde_json::from_str(text).ok()?;
            joined(doc.get("teardown")?)
        }
        _ => None,
    }
}

/// The folder `script` runs in, under the checkout at `tree`: its own `dir` when that stays
/// inside, else the checkout.
#[must_use]
pub fn folder(tree: &Path, script: &RunScript) -> std::path::PathBuf {
    let inside = |dir: &&String| {
        Path::new(dir.as_str()).components().all(|c| matches!(c, std::path::Component::Normal(_)))
    };
    script.dir.as_ref().filter(inside).map_or_else(|| tree.to_path_buf(), |dir| tree.join(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tree = tempfile::tempdir().expect("temp");
        for (path, text) in files {
            let at = tree.path().join(path);
            std::fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
            std::fs::write(at, text).expect("write");
        }
        tree
    }

    fn named((from, scripts): (&'static str, Vec<RunScript>)) -> (&'static str, Vec<String>) {
        (from, scripts.into_iter().map(|s| format!("{}: {}", s.name, s.line)).collect())
    }

    /// Real files, as their repositories keep them (each fixture names its source): what a
    /// person of each tool would see on its Run button, and the archive script where one is
    /// kept.
    #[test]
    fn real_files_read_as_their_tools_read_them() {
        let read = |from: &str, text: &str| {
            let tree = tree_with(&[(from, text)]);
            (find(tree.path()).map(named), archive(tree.path()).map(|f| f.script))
        };
        // github.com/elie222/inbox-zero, `.conductor/settings.toml`.
        let (run, archived) =
            read(".conductor/settings.toml", include_str!("run_fixtures/inbox-zero.settings.toml"));
        let evals = "evals: EVAL_MODELS=deepseek-v4-flash-azure EVAL_RESULT_CACHE=readwrite pnpm --filter inbox-zero-ai test-ai __tests__/eval";
        assert_eq!(
            run,
            Some((
                ".conductor/settings.toml",
                vec!["dev: pnpm dev-setup dev --url conductor".to_owned(), evals.to_owned()]
            ))
        );
        let clean = "node --disable-warning=MODULE_TYPELESS_PACKAGE_JSON --experimental-strip-types scripts/dev-setup.ts clean";
        assert_eq!(archived.as_deref(), Some(clean));
        // github.com/avo-hq/avo, `conductor.json`.
        let (run, archived) =
            read("conductor.json", include_str!("run_fixtures/avo.conductor.json"));
        let dev = "Run: PORT=$CONDUCTOR_PORT ./bin/dev".to_owned();
        assert_eq!(run, Some(("conductor.json", vec![dev])));
        assert_eq!(archived.as_deref(), Some("./conductor-archive"));
        // github.com/typefully/minimal-twitter, `.codex/environments/environment.toml`.
        let codex = ".codex/environments/environment.toml";
        let (run, archived) =
            read(codex, include_str!("run_fixtures/minimal-twitter.environment.toml"));
        let actions =
            ["Run extension: pnpm dev", "Test: pnpm test", "Lint popup: pnpm --dir popup lint"];
        assert_eq!(run, Some((codex, actions.map(str::to_owned).to_vec())));
        assert_eq!(archived, None, "Codex keeps no archive script");
        // docs.superset.sh/setup-teardown-scripts, its example.
        let (run, archived) =
            read(".superset/config.json", include_str!("run_fixtures/superset-docs.config.json"));
        let superset = vec!["Run: ./.superset/run.sh".to_owned()];
        assert_eq!(run, Some((".superset/config.json", superset)));
        assert_eq!(archived.as_deref(), Some("docker-compose down"));
        // github.com/AdiRishi/expo-uniwind-starter, `t3.json`.
        let (run, archived) =
            read("t3.json", include_str!("run_fixtures/expo-uniwind-starter.t3.json"));
        let t3 = [
            "Run iOS: pnpm ios",
            "API Server: pnpm run server:dev",
            "Check: pnpm run check",
            "Test: pnpm test",
            "Typecheck: pnpm typecheck",
        ];
        assert_eq!(run, Some(("t3.json", t3.map(str::to_owned).to_vec())), "its setup left out");
        assert_eq!(archived, None);
    }

    /// Each tool's run scripts are read in their own form, the default first and hidden ones
    /// left out; the first file that names any wins, and its archive script with it where it
    /// keeps one, else the next file's.
    #[test]
    fn each_tools_run_and_archive_scripts_are_read() {
        let conductor = r#"
[scripts]
setup = "bun install"
archive = "docker compose down"

[scripts.run.test]
command = "bun test"
args = ["--watch", "a b"]

[scripts.run.web]
command = "bun dev"
default = true
options = { cwd = "apps/web" }

[scripts.run.secret]
command = "make secret"
hide = true
"#;
        let tree = tree_with(&[(".conductor/settings.toml", conductor)]);
        let found = find(tree.path());
        let web = found.as_ref().and_then(|(_, s)| s.first()).cloned();
        assert_eq!(
            found.map(named),
            Some((
                ".conductor/settings.toml",
                vec!["web: bun dev".to_owned(), "test: bun test --watch 'a b'".to_owned()]
            ))
        );
        let web = web.expect("web");
        assert_eq!(folder(tree.path(), &web), tree.path().join("apps/web"));
        assert_eq!(
            archive(tree.path()),
            Some(Found {
                from: ".conductor/settings.toml",
                script: "docker compose down".to_owned()
            })
        );

        let codex = "[setup]\nscript = \"\"\n\n[[actions]]\nname = \"Run app\"\ncommand = \"pnpm dev\"\n\n[[actions]]\nname = \"Test\"\ncommand = \"pnpm test\"\n";
        let superset = r#"{"setup":["bun i"],"teardown":["make down","rm -rf tmp"],"run":["./run.sh"],"cwd":"../out"}"#;
        let tree = tree_with(&[
            ("conductor.json", r#"{"scripts":{"setup":"npm ci"}}"#),
            (".codex/environments/environment.toml", codex),
            (".superset/config.json", superset),
        ]);
        assert_eq!(
            find(tree.path()).map(named),
            Some((
                ".codex/environments/environment.toml",
                vec!["Run app: pnpm dev".to_owned(), "Test: pnpm test".to_owned()]
            )),
            "conductor.json names no run script, so Codex's actions are read"
        );
        assert_eq!(
            archive(tree.path()).map(|f| (f.from, f.script)),
            Some((".superset/config.json", "make down && rm -rf tmp".to_owned()))
        );

        let tree = tree_with(&[(".superset/config.json", superset)]);
        let (_, scripts) = find(tree.path()).expect("superset's run");
        let run = scripts.first().expect("one");
        assert_eq!((run.name.as_str(), run.line.as_str()), ("Run", "./run.sh"));
        assert_eq!(folder(tree.path(), run), tree.path(), "a folder climbing out is not taken");

        let t3 = r#"{"scripts":[{"name":"Setup","command":"bun i","runOnWorktreeCreate":true},{"name":"Dev","command":"bun dev"}]}"#;
        let tree = tree_with(&[("t3.json", t3), ("conductor.json", r#"{"scripts":{"run":""}}"#)]);
        assert_eq!(
            find(tree.path()).map(named),
            Some(("t3.json", vec!["Dev: bun dev".to_owned()]))
        );
        assert_eq!(archive(tree.path()), None);

        let tree = tree_with(&[("conductor.json", "{not json")]);
        assert_eq!(find(tree.path()).map(named), None, "a file that does not parse names none");
    }

    /// What the client opens: each script through the person's login shell, which takes the
    /// terminal over once it ends, in its folder, with the places in its environment under
    /// Slopty's names, Conductor's and its tool's own. A checkout that keeps none says so.
    #[test]
    fn a_run_script_is_ready_to_open() {
        let tree = tree_with(&[(".superset/config.json", r#"{"run":["bun dev"],"cwd":"web"}"#)]);
        let found = scripts(tree.path());
        assert_eq!(found.from.as_deref(), Some(".superset/config.json"));
        let [run] = found.list.as_slice() else { panic!("{found:?}") };
        let shell = super::super::script::login_shell();
        assert_eq!(run.command, super::super::script::command_line("bun dev", &shell));
        assert_eq!((run.name.as_str(), run.line.as_str()), ("Run", "bun dev"));
        assert_eq!(run.cwd, tree.path().join("web").to_string_lossy());
        let place = tree.path().to_string_lossy().into_owned();
        let env = |key: &str| run.env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        assert_eq!(env("SLOPTY_WORKSPACE_PATH"), Some(place.clone()));
        assert_eq!(env("CONDUCTOR_WORKSPACE_PATH"), Some(place.clone()));
        assert_eq!(env("SUPERSET_WORKSPACE_PATH"), Some(place));

        let none = tree_with(&[]);
        assert_eq!(
            scripts(none.path()),
            slopty_proto::git::RunScripts { from: None, list: Vec::new() }
        );
    }
}
