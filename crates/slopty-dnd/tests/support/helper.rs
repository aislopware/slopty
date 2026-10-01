//! The drag helper's roles as a test app, for the live tests of `tests/roles.rs`, which is its
//! parent and the only process that posts to it: `slopty_dnd`'s source or catcher at a point,
//! as the worker's helper runs them, saying what each drag did.
//!
//! `--role source --at x,y` waits with a drag of one item per `--file <path>` (whole), per
//! `--later-text <s>` (text given when a target reads it) and per `--later-file
//! <path>:<bytes>:<ms>` (a file written `<ms>` after a target asks for its URL, standing for an
//! upload that finishes at the drop). It says `began`, `provide item=… type=… waited_ms=…`, and
//! `ended op=…`.
//!
//! `--role catcher --at x,y --dir <d> --max <bytes>` waits for a drop, calling promises into
//! `<d>`. It says `entered`, `caught files=… data=… too_big=… promises=…`, then `promised
//! path=… size=…` or `promised error=…` per promise.
//!
//! Both first say `ready pid=<pid> window=<CGWindowID>`, never make the app active, and leave
//! when stdin closes.

#[cfg(target_os = "macos")]
#[path = "child.rs"]
#[expect(dead_code, reason = "shared with the other apps, which use the rest of it")]
mod child;

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use objc2::MainThreadMarker;
    use slopty_dnd::catcher::{Catcher, CatcherEvents, Caught, Promised};
    use slopty_dnd::items::{Item, Provide, file_url_bytes};
    use slopty_dnd::source::{Source, SourceEvents};

    use crate::child::{Args, say};

    struct Said;

    impl SourceEvents for Said {
        fn began(&self, (x, y): (f64, f64)) {
            say(&format!("began x={x:.0} y={y:.0}"));
        }

        fn moved(&self, _at: (f64, f64)) {}

        fn ended(&self, operation: u64, (x, y): (f64, f64)) {
            say(&format!("ended op={operation} x={x:.0} y={y:.0}"));
        }
    }

    impl CatcherEvents for Said {
        fn entered(&self) {
            say("entered");
        }

        fn caught(&self, caught: Caught) {
            let files: Vec<String> =
                caught.files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
            let data: Vec<String> =
                caught.data.iter().map(|(n, t, b)| format!("{n}:{t}:{}", b.len())).collect();
            let too_big: Vec<String> =
                caught.too_big.iter().map(|(n, t, s)| format!("{n}:{t}:{s}")).collect();
            say(&format!(
                "caught files={} data={} too_big={} promises={}",
                files.join("|"),
                data.join(","),
                too_big.join(","),
                caught.promises
            ));
        }

        fn promised(&self, promised: Promised) {
            match promised {
                Ok(path) => {
                    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
                    say(&format!("promised path={} size={size}", path.display()));
                }
                Err(error) => say(&format!("promised error={error}")),
            }
        }
    }

    /// `--at x,y`.
    fn point(args: &Args) -> (f64, f64) {
        let parts: Vec<f64> =
            args.get("at").unwrap_or("0,0").split(',').filter_map(|v| v.parse().ok()).collect();
        (parts.first().copied().unwrap_or(0.0), parts.get(1).copied().unwrap_or(0.0))
    }

    /// Wait out an upload that is still arriving.
    #[expect(clippy::disallowed_methods, reason = "a test app standing for a slow upload")]
    fn wait(for_: Duration) {
        std::thread::sleep(for_);
    }

    /// The source's items and what answers their promises.
    fn items(args: &Args) -> (Vec<Item>, Provide) {
        let mut items: Vec<Item> =
            args.all("file").iter().map(|f| Item::File(PathBuf::from(f))).collect();
        let texts: Vec<(usize, String)> = args
            .all("later-text")
            .iter()
            .map(|t| {
                items.push(Item::Later(vec!["public.utf8-plain-text".to_owned()]));
                (items.len().saturating_sub(1), t.clone())
            })
            .collect();
        let files: Vec<(usize, PathBuf, usize, u64)> = args
            .all("later-file")
            .iter()
            .filter_map(|spec| {
                let mut parts = spec.rsplitn(3, ':');
                let ms = parts.next()?.parse().ok()?;
                let bytes = parts.next()?.parse().ok()?;
                let path = PathBuf::from(parts.next()?);
                items.push(Item::Later(vec!["public.file-url".to_owned()]));
                Some((items.len().saturating_sub(1), path, bytes, ms))
            })
            .collect();
        let provide: Provide = Arc::new(move |item, uti| {
            let asked = Instant::now();
            let answer = if let Some((_, text)) = texts.iter().find(|(n, _)| *n == item) {
                Some(text.clone().into_bytes())
            } else if let Some((_, path, bytes, ms)) = files.iter().find(|(n, ..)| *n == item) {
                wait(Duration::from_millis(*ms));
                std::fs::write(path, vec![0x5a; *bytes]).ok().map(|()| file_url_bytes(path))
            } else {
                None
            };
            say(&format!(
                "provide item={item} type={uti} waited_ms={} given={}",
                asked.elapsed().as_millis(),
                u8::from(answer.is_some())
            ));
            answer
        });
        (items, provide)
    }

    /// Say `ready`, then leave when the parent closes stdin or goes.
    fn ready(window: isize) {
        say(&format!("ready pid={} window={window}", std::process::id()));
        std::thread::spawn(|| {
            let _read = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
            #[expect(clippy::exit, reason = "a test app ends when its parent lets go of it")]
            std::process::exit(0);
        });
    }

    pub fn run() -> ExitCode {
        let Some(mtm) = MainThreadMarker::new() else { return ExitCode::FAILURE };
        let args = Args::read();
        let app = slopty_dnd::window::application(mtm);
        let at = point(&args);
        // Held until the app ends.
        let _source;
        let _catcher;
        if args.get("role") == Some("catcher") {
            let max = args.get("max").and_then(|m| m.parse().ok()).unwrap_or(1 << 20);
            let catcher = Catcher::new(mtm, max, Arc::new(Said));
            let dir = PathBuf::from(args.get("dir").unwrap_or("/tmp"));
            catcher.at(at, dir);
            ready(catcher.window_number());
            _catcher = catcher;
        } else {
            let source = Source::new(mtm, Box::new(Said));
            let (items, provide) = items(&args);
            source.at(at, &items, &provide);
            ready(source.window_number());
            _source = source;
        }
        app.run();
        ExitCode::SUCCESS
    }
}
