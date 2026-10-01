//! The editor and the browser tile in the real app: a file on the worker edited, saved and
//! caught changing on disk under an edit; a page the test serves itself, opened in a tile,
//! read back from the web view and hidden while the palette is over it; a `_blank` link's tile
//! beside it, its script's dialogs as sheets, and find's count on it. Each state worth a look
//! is a golden.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::time::Duration;

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::snapshot::{MAC_TOLERANCE as TOLERANCE, assert_matches};
use slopty_e2e::{Command, Driver, Dump, FileItemInfo, Stack};

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(20);
/// The renders' window: the size the other app goldens use.
const WINDOW: (f32, f32) = (900.0, 600.0);

/// Render the frame after every link has its round trip, every shell at a prompt has its
/// caret there and two dumps agree on every tile's place, and hold it against
/// `golden/<name>.png`.
async fn golden(drv: &mut Driver, stack_dir: &std::path::Path, name: &str) {
    drv.wait_for("the first round trip", STEP, Dump::rtt_sampled).await.unwrap();
    let mut last =
        drv.wait_for("the carets at their prompts", STEP, Dump::prompts_settled).await.unwrap();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(120)).await;
        let next = drv.dump().await.unwrap();
        let same = next.items.len() == last.items.len()
            && next
                .items
                .iter()
                .zip(&last.items)
                .all(|(a, b)| a.bounds.iter().zip(&b.bounds).all(|(x, y)| (x - y).abs() < 0.5));
        last = next;
        if same {
            break;
        }
    }
    let frame = drv.render(&stack_dir.join(format!("{name}.png"))).await.unwrap();
    assert_matches(name, &frame, TOLERANCE, &artifacts_dir()).unwrap();
}

/// The first shell, connected and prompted.
async fn first_shell(drv: &mut Driver) -> Dump {
    drv.wait_for("the first shell with a prompt", STEP, |d| {
        d.status == "connected"
            && d.focus.as_deref() == Some("terminal")
            && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
    })
    .await
    .unwrap()
}

fn file_of(d: &Dump) -> Option<&FileItemInfo> {
    d.item("file").and_then(|i| i.file.as_ref())
}

/// Open a file on the worker, edit it and ⌘S: the disk has the edit (its final newline kept).
/// Edit again, change the file behind the tile's back: the tile says so inline and keeps the
/// edit; "Reload" takes the disk's text.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_file_is_edited_saved_and_caught_changing_under_an_edit() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let file = project.join("main.rs");
    std::fs::write(&file, "fn main() {\n    println!(\"hello\");\n}\n").unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;

    drv.ok(&Command::OpenFile { path: file.display().to_string(), line: None }).await.unwrap();
    let dump = drv
        .wait_for("the file in its editor, with the keyboard", STEP, |d| {
            file_of(d).is_some_and(|f| f.lines == 3 && f.summary.contains("Rust"))
                && d.focused.starts_with("file:")
        })
        .await
        .unwrap();
    let info = file_of(&dump).unwrap();
    assert!(!info.edited && info.trouble.is_none() && info.read_only.is_none(), "{info:?}");
    assert!(dump.a11y_node("Heading", Some("file main.rs")).is_some(), "{:#?}", dump.a11y);
    golden(drv, &dir, "editor").await;

    drv.type_text("// edited\n").await.unwrap();
    let dump = drv
        .wait_for("the edit, unsaved", STEP, |d| file_of(d).is_some_and(|f| f.edited))
        .await
        .unwrap();
    assert!(dump.a11y_node("Image", Some("Unsaved changes")).is_some(), "{:#?}", dump.a11y);
    golden(drv, &dir, "editor-dirty").await;

    drv.keys("cmd-s").await.unwrap();
    drv.wait_for("the save", STEP, |d| file_of(d).is_some_and(|f| !f.edited)).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "// edited\nfn main() {\n    println!(\"hello\");\n}\n",
        "the disk has the edit and the final newline"
    );

    // Another edit, and the file changes on disk before it is saved.
    drv.type_text("x").await.unwrap();
    drv.wait_for("a second edit", STEP, |d| file_of(d).is_some_and(|f| f.edited)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::write(&file, "fn main() {}\n").unwrap();
    let dump = drv
        .wait_for("the conflict", STEP, |d| {
            file_of(d).is_some_and(|f| f.trouble.as_deref() == Some("conflict"))
        })
        .await
        .unwrap();
    assert!(file_of(&dump).unwrap().edited, "the edit is kept");
    for button in ["Reload", "Overwrite"] {
        assert!(dump.a11y_node("Button", Some(button)).is_some(), "{button}: {:#?}", dump.a11y);
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn main() {}\n", "nothing written");
    golden(drv, &dir, "editor-conflict").await;

    let reload = dump.a11y_node("Button", Some("Reload")).unwrap().bounds;
    drv.click(reload[0] + reload[2] / 2.0, reload[1] + reload[3] / 2.0).await.unwrap();
    let dump = drv
        .wait_for("the disk's text, clean", STEP, |d| {
            file_of(d).is_some_and(|f| !f.edited && f.trouble.is_none() && f.lines == 1)
        })
        .await
        .unwrap();
    assert!(dump.a11y_node("Button", Some("Reload")).is_none(), "{:#?}", dump.a11y);
    stack.shutdown().await;
}

/// A 20 000-line file, ten times the old viewer's limit and past the inline limit, so it comes
/// down a bulk stream and its save goes up one: the tile opens it editable near its end, takes
/// an edit there, and ⌘S puts the whole edited file on the worker's disk.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_twenty_thousand_line_file_is_edited_near_its_end_and_saved() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let file = stack.dir.path().join("big.rs");
    let lines: Vec<String> =
        (0..20_000).map(|i| format!("fn line_{i}() -> u32 {{ {i} * 2 + 1 }}")).collect();
    let body = format!("{}\n", lines.join("\n"));
    assert!(body.len() > slopty_proto::file::INLINE_FILE_BYTES, "it must stream");
    std::fs::write(&file, &body).unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;

    drv.open_file(&file.display().to_string(), Some(19_990)).await.unwrap();
    let dump = drv
        .wait_for("the whole file in its editor, at line 19 990, with the keyboard", STEP, |d| {
            file_of(d).is_some_and(|f| f.lines == 20_000 && f.line == Some(19_990))
                && d.focused.starts_with("file:")
        })
        .await
        .unwrap();
    let info = file_of(&dump).unwrap();
    assert!(!info.edited && info.trouble.is_none() && info.read_only.is_none(), "{info:?}");

    drv.type_text("// edited near the end\n").await.unwrap();
    drv.wait_for("the edit, unsaved", STEP, |d| file_of(d).is_some_and(|f| f.edited))
        .await
        .unwrap();
    drv.keys("cmd-s").await.unwrap();
    drv.wait_for("the save", STEP, |d| {
        file_of(d).is_some_and(|f| !f.edited && f.trouble.is_none() && f.lines == 20_001)
    })
    .await
    .unwrap();
    let mut expected = lines;
    expected.insert(19_989, "// edited near the end".to_owned());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("{}\n", expected.join("\n")),
        "the worker's file is the whole edited text, its final newline kept"
    );
    stack.shutdown().await;
}

/// An edit made in a file tile and never saved survives the app being killed mid-edit
/// (SIGKILL: no quit, no goodbye, as a crash or a dead battery would leave it). Relaunched, the
/// tile on the same file takes the edit back, unsaved, and the file on the worker is as it was.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn an_unsaved_edit_survives_the_app_being_killed() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let file = stack.dir.path().join("notes.md");
    std::fs::write(&file, "# Notes\n").unwrap();
    let kept = stack.dir.path().join("app").join("unsaved");
    first_shell(&mut stack.driver).await;
    let drv = &mut stack.driver;
    drv.open_file(&file.display().to_string(), Some(1)).await.unwrap();
    drv.wait_for("the file in its editor, with the keyboard", STEP, |d| {
        file_of(d).is_some_and(|f| f.text.as_deref() == Some("# Notes"))
            && d.focused.starts_with("file:")
    })
    .await
    .unwrap();
    drv.type_text("draft ").await.unwrap();
    drv.wait_for("the edit, unsaved", STEP, |d| {
        file_of(d).is_some_and(|f| f.edited && f.text.as_deref() == Some("draft # Notes"))
    })
    .await
    .unwrap();
    // Kept within a moment of the keystroke: the kill comes once it is on this device's disk,
    // and before anything else could write it.
    let started = std::time::Instant::now();
    while std::fs::read_dir(&kept).map_or(0, Iterator::count) == 0 && started.elapsed() < STEP {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    println!(
        "MEASURE hot exit: the edit was kept {} ms after its echo",
        started.elapsed().as_millis()
    );

    stack.kill_app().await.unwrap();
    stack.relaunch_app().await.unwrap();
    let drv = &mut stack.driver;
    let dump = drv
        .wait_for("the edit back on its tile, unsaved", STEP, |d| {
            file_of(d).is_some_and(|f| f.edited && f.text.as_deref() == Some("draft # Notes"))
        })
        .await
        .unwrap();
    let info = file_of(&dump).unwrap();
    assert!(info.trouble.is_none(), "the disk did not move: no conflict: {info:?}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "# Notes\n", "nothing saved on its own");
    stack.shutdown().await;
}

/// "About Slopty" from the palette: the panel leads with the mark, its cursor lit while the
/// worker is reachable (steady: the self-test runs under Reduce Motion), then the name and the
/// version. Esc gives the workspace back.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn about_slopty_leads_with_the_mark() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.keys("cmd-shift-p").await.unwrap();
    drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
        .await
        .unwrap();
    drv.type_text("About Slopty").await.unwrap();
    drv.keys("enter").await.unwrap();
    let about = |d: &Dump| d.a11y_node("Dialog", Some("About Slopty")).is_some();
    drv.wait_for("the About panel", STEP, about).await.unwrap();
    golden(drv, &dir, "about").await;
    drv.keys("escape").await.unwrap();
    drv.wait_for("the workspace back", STEP, |d| !about(d)).await.unwrap();
    stack.shutdown().await;
}

/// The test page, served on localhost by the test itself, to every request.
const PAGE: &str = "<!doctype html><html><head><meta charset=utf-8>\
<title>Slopty test page</title></head>\
<body style=\"margin:0;font:15px -apple-system,sans-serif;background:#fafafa;color:#1c1c1e\">\
<div style=\"padding:32px\"><h1 style=\"font-size:22px;margin:0 0 8px\">Served by the test</h1>\
<p style=\"margin:0;color:#6e6e73\">A page on localhost, in a browser tile.</p></div>\
</body></html>";

/// A field with an edit already made in it, its value and selection length in the title,
/// so the test reads what an edit key did from the web view's title.
const EDIT_PAGE: &str = "<!doctype html><html><head><meta charset=utf-8><title>edit</title>\
</head><body><input id=f value=abc><script>\
const f=document.getElementById('f');\
const show=()=>{document.title='v='+f.value+'|s='+(f.selectionEnd-f.selectionStart);};\
f.addEventListener('input',show);document.addEventListener('selectionchange',show);\
f.focus();f.setSelectionRange(3,3);document.execCommand('insertText',false,'xyz');show();\
</script></body></html>";

/// A page whose `_blank` link follows itself as it loads, as a click on it would: the page's
/// words hold "apple" three times over, in three cases, for find to count.
const POPUP_PAGE: &str = "<!doctype html><html><head><meta charset=utf-8><title>popup</title>\
</head><body><p>An apple, an Apple and APPLE pie.</p>\
<a id=l href=\"/second\" target=_blank>second</a>\
<script>document.getElementById('l').click();</script></body></html>";

/// The page the link opens: an `alert`, then, once that is answered, a `confirm` whose OK
/// closes the page's window.
const SECOND_PAGE: &str = "<!doctype html><html><head><meta charset=utf-8><title>second</title>\
</head><body><script>setTimeout(() => {\
alert('Hello from the second page');\
if (confirm('Close this page?')) { window.close(); } }, 0);</script></body></html>";

/// Serve [`PAGE`], [`EDIT_PAGE`] at `/edit`, [`POPUP_PAGE`] at `/popup` and [`SECOND_PAGE`] at
/// `/second`, on an ephemeral localhost port until the process ends; the port.
fn serve_page() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            let _read = reader.read_line(&mut line);
            let page = match line.split(' ').nth(1) {
                Some("/edit") => EDIT_PAGE,
                Some("/popup") => POPUP_PAGE,
                Some("/second") => SECOND_PAGE,
                _ => PAGE,
            };
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
                page.len()
            );
            let _sent = (&stream).write_all(answer.as_bytes());
        }
    });
    port
}

/// A page the test serves opens in a tile: the item names the worker's port, which is taken
/// here by the test's own server, so the client serves it on another and the page loads
/// through the tunnel. The web view reports its title and address, the tile's header says
/// them, and the page stays on screen with the palette drawn over it. A click in the page
/// gives it the keyboard, and the edit keys then reach the page through AppKit.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_page_on_localhost_opens_in_a_browser_tile() {
    let port = serve_page();
    let url = format!("http://127.0.0.1:{port}/");
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;

    drv.ok(&Command::OpenUrl { url: url.clone() }).await.unwrap();
    let dump = drv
        .wait_for("the page, loaded, shown and named in its header", STEP, |d| {
            d.item("browser").and_then(|i| i.browser.as_ref()).is_some_and(|b| {
                b.title == "Slopty test page" && !b.loading && b.shown && b.snapshot
            }) && d.a11y_node("Heading", Some("browser Slopty test page")).is_some()
        })
        .await
        .unwrap();
    let page = dump.item("browser").and_then(|i| i.browser.clone()).unwrap();
    assert_eq!(page.url, url, "the item keeps the worker's address");
    let local = page.local_url.clone().unwrap();
    assert!(local.starts_with("http://127.0.0.1:") && local != url, "served elsewhere: {local}");
    assert_eq!(page.page_url, url, "the web view's address, on the worker's port");
    assert!(page.failed.is_none(), "{page:?}");
    golden(drv, &dir, "browser").await;

    drv.keys("cmd-shift-p").await.unwrap();
    drv.wait_for("the palette over the page, which stays", STEP, |d| {
        d.a11y_node("Dialog", Some("Commands")).is_some()
            && d.item("browser").and_then(|i| i.browser.as_ref()).is_some_and(|b| b.shown)
    })
    .await
    .unwrap();
    drv.keys("escape").await.unwrap();
    drv.wait_for("the palette gone", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_none())
        .await
        .unwrap();

    // The edit keys reach the page, not the workspace: undo and redo the page's own edit,
    // then select the field's text. (Copy, cut and paste take the same path; they are left
    // out here because they would touch this machine's clipboard.)
    let edit = format!("http://127.0.0.1:{port}/edit");
    drv.ok(&Command::OpenUrl { url: edit.clone() }).await.unwrap();
    let title_of = |d: &Dump| {
        d.items
            .iter()
            .filter_map(|i| i.browser.as_ref())
            .find(|b| b.url == edit)
            .map(|b| b.title.clone())
            .unwrap_or_default()
    };
    let dump = drv
        .wait_for("the edit page, its edit made", STEP, |d| title_of(d).starts_with("v=abcxyz|"))
        .await
        .unwrap();
    let body = dump.a11y_node("Document", Some(&format!("Page {edit}"))).unwrap().bounds;
    let (x, y) = (body[0] + body[2] / 2.0, body[1] + body[3] / 2.0);
    drv.click(x, y).await.unwrap();
    let page_id = dump.items.iter().find(|i| i.browser.as_ref().is_some_and(|b| b.url == edit));
    let holder = format!("browser:{}", page_id.unwrap().id);
    drv.wait_for("the page holds the keyboard", STEP, |d| d.focused == holder).await.unwrap();
    for (keys, want) in
        [("cmd-z", "v=abc|"), ("cmd-shift-z", "v=abcxyz|"), ("cmd-a", "v=abcxyz|s=6")]
    {
        drv.ok(&Command::PageKeys { url: edit.clone(), keys: keys.to_owned() }).await.unwrap();
        drv.wait_for(keys, STEP, |d| title_of(d).starts_with(want)).await.unwrap();
    }
    stack.shutdown().await;
}

/// A page on a host only the worker names loads through the worker's proxy as it is, its
/// address never rewritten, and the worker's own word says why when it cannot reach one. The
/// worker's resolver answers `*.localhost` with its loopback, and the page's network never
/// resolves a proxied name itself: the refusal the first page shows, which only the worker can
/// give, proves the path the last one loads by.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_page_on_a_host_only_the_worker_names_loads_through_its_proxy() {
    let dead = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let port = serve_page();
    let refused = format!("http://only-the-worker.localhost:{dead}/");
    let unresolved = "http://nowhere.invalid/".to_owned();
    let named = format!("http://only-the-worker.localhost:{port}/");
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    let page_of = |d: &Dump, url: &str| {
        d.items.iter().filter_map(|i| i.browser.as_ref()).find(|b| b.url == url).cloned()
    };

    let says = [
        (&refused, format!("nothing listens on port {dead} of only-the-worker.localhost")),
        (&unresolved, "the worker finds no host named nowhere.invalid".to_owned()),
    ];
    for (url, why) in says {
        drv.ok(&Command::OpenUrl { url: url.clone() }).await.unwrap();
        drv.wait_for(&format!("{url} says why"), STEP, |d| {
            page_of(d, url).and_then(|b| b.failed).as_deref() == Some(why.as_str())
        })
        .await
        .unwrap();
    }

    let asked = std::time::Instant::now();
    drv.ok(&Command::OpenUrl { url: named.clone() }).await.unwrap();
    let dump = drv
        .wait_for("the named page, loaded", STEP, |d| {
            page_of(d, &named).is_some_and(|b| b.title == "Slopty test page" && !b.loading)
        })
        .await
        .unwrap();
    let took = asked.elapsed();
    let page = page_of(&dump, &named).unwrap();
    assert_eq!(page.local_url.as_deref(), Some(named.as_str()), "loaded as it is");
    assert_eq!(page.page_url, named, "the web view's address is the worker's");
    assert!(page.failed.is_none(), "{page:?}");
    eprintln!("the named page loaded {took:?} after it was asked for");

    let root = stack.dir.path().to_path_buf();
    let store = slopty_e2e::harness::page_store(&root).expect("the worker's id");
    assert!(store.exists(), "the worker's pages keep a store of their own: {}", store.display());
    let id = slopty_e2e::harness::worker_id(&root).unwrap();
    stack.driver.ok(&Command::ForgetWorker { id }).await.unwrap();
    let deadline = std::time::Instant::now() + STEP;
    while store.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!store.exists(), "a forgotten worker's pages keep nothing: {}", store.display());
    stack.shutdown().await;
}

/// The browser tiles of the dump, in the order the workspace lists them.
fn pages(d: &Dump) -> Vec<&slopty_e2e::ItemInfo> {
    d.items.iter().filter(|i| i.kind == "browser").collect()
}

/// A page's `_blank` link opens a second page tile right of the first, on the worker's port.
/// That page's `alert` is a sheet in its tile, read from the accessibility tree, and ↩ answers
/// it; its `confirm` follows, and OK runs the page's `window.close()`, which closes the tile.
/// Back on the first page, ⌘F finds in it and the bar says how many matches the page holds.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_blank_link_opens_a_tile_and_a_script_s_dialogs_are_sheets_in_it() {
    let port = serve_page();
    let popup = format!("http://127.0.0.1:{port}/popup");
    let second = format!("http://127.0.0.1:{port}/second");
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;

    drv.ok(&Command::OpenUrl { url: popup.clone() }).await.unwrap();
    let dump = drv
        .wait_for("the link's page in a tile of its own", STEP, |d| {
            pages(d).iter().any(|i| i.browser.as_ref().is_some_and(|b| b.url == second))
        })
        .await
        .unwrap();
    let opener = pages(&dump).into_iter().find(|i| i.browser.as_ref().unwrap().url == popup);
    let opened = pages(&dump).into_iter().find(|i| i.browser.as_ref().unwrap().url == second);
    let (opener, opened) = (opener.unwrap(), opened.unwrap());
    assert_eq!(opened.pos[1], opener.pos[1] + 1, "right of its opener: {opener:?} {opened:?}");

    let alert = "Hello from the second page";
    drv.wait_for("the alert, a sheet in the tile", STEP, |d| {
        d.a11y_node("AlertDialog", Some(alert)).is_some()
    })
    .await
    .unwrap();
    let dump = drv.dump().await.unwrap();
    assert!(dump.a11y_node("Button", Some("OK")).is_some(), "{:#?}", dump.a11y);
    assert!(dump.a11y_node("Button", Some("Cancel")).is_none(), "an alert has only OK");

    drv.keys("enter").await.unwrap();
    drv.wait_for("the alert answered, the page on to its confirm", STEP, |d| {
        d.a11y_node("AlertDialog", Some(alert)).is_none()
            && d.a11y_node("AlertDialog", Some("Close this page?")).is_some()
    })
    .await
    .unwrap();
    let dump = drv.dump().await.unwrap();
    assert!(dump.a11y_node("Button", Some("Cancel")).is_some(), "{:#?}", dump.a11y);

    drv.keys("enter").await.unwrap();
    let dump = drv
        .wait_for("the page closed its window, and its tile went", STEP, |d| {
            d.a11y_node("AlertDialog", None).is_none() && pages(d).len() == 1
        })
        .await
        .unwrap();
    assert_eq!(pages(&dump)[0].browser.as_ref().unwrap().url, popup);

    // The opener again, focused, and ⌘F in it: its text holds three matches.
    drv.ok(&Command::OpenUrl { url: popup.clone() }).await.unwrap();
    drv.wait_for("the opener focused", STEP, |d| pages(d).first().is_some_and(|i| i.active))
        .await
        .unwrap();
    drv.keys("cmd-f").await.unwrap();
    drv.wait_for("the find bar", STEP, |d| d.a11y_node("Group", Some("Find in page")).is_some())
        .await
        .unwrap();
    drv.type_text("apple").await.unwrap();
    drv.wait_for("three matches", STEP, |d| {
        d.a11y_node("Label", Some("Matches")).and_then(|n| n.value.as_deref()) == Some("3 matches")
    })
    .await
    .unwrap();
    drv.keys("cmd-g").await.unwrap();
    drv.keys("escape").await.unwrap();
    drv.wait_for("the bar closed", STEP, |d| d.a11y_node("Group", Some("Find in page")).is_none())
        .await
        .unwrap();
    stack.shutdown().await;
}

/// The labels of the folder rows the dump's accessibility tree holds, top to bottom.
fn folder_rows(d: &Dump) -> Vec<String> {
    d.a11y.iter().filter(|n| n.role == "ListBoxOption").filter_map(|n| n.label.clone()).collect()
}

/// The directory the folder tile says it is at.
fn folder_at(d: &Dump) -> Option<String> {
    d.a11y
        .iter()
        .filter(|n| n.role == "Group")
        .find_map(|n| n.label.as_deref()?.strip_prefix("Folder ").map(str::to_owned))
}

/// "Open folder…" in the palette opens a folder tile at the shell's directory, with the
/// keyboard: its folders first, a hidden entry listed too. ↩ on a folder browses into it in
/// place; a file written there shows in the tile unasked; and ↩ on a file there opens a file
/// tile right of the folder with the file's text.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_folder_tile_browses_the_worker_and_opens_a_file_beside_it() {
    let mut stack = Stack::launch("e2e-folder").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let project = stack.path("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("README.md"), "# Project\n").unwrap();
    std::fs::write(project.join(".env"), "KEY=1\n").unwrap();
    std::fs::write(project.join("src/main.rs"), "fn main() {\n    run();\n}\n").unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.type_text(&format!("cd '{}' && pwd", project.display())).await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the shell in the project", STEP, |d| {
        d.rows_containing("project").iter().any(|r| r.trim().ends_with("/project"))
    })
    .await
    .unwrap();

    drv.keys("cmd-shift-p").await.unwrap();
    drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
        .await
        .unwrap();
    drv.type_text("Open folder").await.unwrap();
    drv.keys("enter").await.unwrap();
    // The palette again, its field holding the shell's directory: ↩ opens it.
    drv.wait_for("the palette at the shell's directory", STEP, |d| {
        d.a11y.iter().any(|n| {
            n.role == "ListBoxOption"
                && n.label
                    .as_deref()
                    .is_some_and(|l| l.contains("Open folder") && l.contains("/project"))
        })
    })
    .await
    .unwrap();
    drv.keys("enter").await.unwrap();
    let dump = drv
        .wait_for("the folder tile, listed, with the keyboard", STEP, |d| {
            d.item("folder").is_some()
                && d.focused.starts_with("folder:")
                && folder_rows(d) == ["src", ".env", "README.md"]
        })
        .await
        .unwrap();
    let at = folder_at(&dump).unwrap();
    assert!(at.ends_with("/project"), "{at}");
    // Titled by its name, as the shell beside it is by the same directory: two kinds, so no
    // number tells them apart.
    assert!(dump.a11y_node("Heading", Some("folder project")).is_some(), "{:#?}", dump.a11y);
    assert!(dump.a11y_node("Heading", Some("terminal project")).is_some(), "{:#?}", dump.a11y);
    assert!(dump.a11y_node("Button", Some("Enclosing folder")).is_some(), "{:#?}", dump.a11y);
    golden(drv, &dir, "folder").await;

    drv.keys("enter").await.unwrap();
    let dump = drv
        .wait_for("the folder moved into src", STEP, |d| {
            folder_at(d).is_some_and(|at| at.ends_with("/project/src"))
                && folder_rows(d) == ["main.rs"]
        })
        .await
        .unwrap();
    assert!(dump.focused.starts_with("folder:"), "the keyboard stays: {}", dump.focused);

    // A file written there, as an agent would, shows in the tile unasked; the selection stays
    // on the entry it was on.
    let written = std::time::Instant::now();
    std::fs::write(project.join("src/lib.rs"), "pub fn run() {}\n").unwrap();
    drv.wait_for("lib.rs listed without a refresh", STEP, |d| {
        folder_rows(d) == ["lib.rs", "main.rs"]
    })
    .await
    .unwrap();
    let took = written.elapsed();
    println!("a file written → its folder tile's row: {took:?} (dumps polled)");
    assert!(took < Duration::from_secs(2), "followed, not refreshed on focus: {took:?}");

    drv.keys("enter").await.unwrap();
    let dump = drv
        .wait_for("main.rs in a file tile", STEP, |d| {
            file_of(d).is_some_and(|f| f.path.ends_with("/project/src/main.rs") && f.lines == 3)
        })
        .await
        .unwrap();
    let folder = dump.item("folder").unwrap();
    let file = dump.item("file").unwrap();
    assert_eq!(file.pos[1], folder.pos[1] + 1, "right of the folder: {folder:?} {file:?}");
    assert!(file.active, "the file tile has the focus");
    stack.shutdown().await;
}
