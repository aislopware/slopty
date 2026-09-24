//! The editor and the browser tile in the real app: a file on the worker edited, saved and
//! caught changing on disk under an edit; a page the test serves itself, opened in a tile,
//! read back from the web view and hidden while the palette is over it. Each state worth a
//! look is a golden.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::time::Duration;

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::snapshot::assert_matches;
use slopty_e2e::{Command, Driver, Dump, FileItemInfo, Stack};

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(20);
/// The renders' window: the size the other app goldens use.
const WINDOW: (f32, f32) = (900.0, 600.0);
/// Fraction of pixels allowed to differ from a golden.
const TOLERANCE: f64 = 0.01;

fn gated() -> bool {
    if std::env::var_os("SLOPTY_APP_E2E").is_none() {
        eprintln!("skipped: set SLOPTY_APP_E2E=1 (or run `cargo xtask e2e app`)");
        return false;
    }
    true
}

/// Render the frame after two dumps agree on every tile's place, and hold it against
/// `golden/<name>.png`.
async fn golden(drv: &mut Driver, stack_dir: &std::path::Path, name: &str) {
    let mut last = drv.dump().await.unwrap();
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
async fn a_file_is_edited_saved_and_caught_changing_under_an_edit() {
    if !gated() {
        return;
    }
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
    assert!(
        dump.a11y_node("Heading", Some("file main.rs · project")).is_some(),
        "{:#?}",
        dump.a11y
    );
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

/// Serve [`PAGE`], and [`EDIT_PAGE`] at `/edit`, on an ephemeral localhost port until the
/// process ends; the port.
fn serve_page() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            let _read = reader.read_line(&mut line);
            let page = if line.starts_with("GET /edit ") { EDIT_PAGE } else { PAGE };
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
/// them, and the page hides while the palette is over it.
#[tokio::test]
async fn a_page_on_localhost_opens_in_a_browser_tile() {
    if !gated() {
        return;
    }
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
    drv.wait_for("the palette over a hidden page", STEP, |d| {
        d.a11y_node("Dialog", Some("Commands")).is_some()
            && d.item("browser").and_then(|i| i.browser.as_ref()).is_some_and(|b| !b.shown)
    })
    .await
    .unwrap();
    drv.keys("escape").await.unwrap();
    drv.wait_for("the page back", STEP, |d| {
        d.a11y_node("Dialog", Some("Commands")).is_none()
            && d.item("browser").and_then(|i| i.browser.as_ref()).is_some_and(|b| b.shown)
    })
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
    drv.wait_for("the edit page, its edit made", STEP, |d| title_of(d).starts_with("v=abcxyz|"))
        .await
        .unwrap();
    for (keys, want) in
        [("cmd-z", "v=abc|"), ("cmd-shift-z", "v=abcxyz|"), ("cmd-a", "v=abcxyz|s=6")]
    {
        drv.ok(&Command::PageKeys { url: edit.clone(), keys: keys.to_owned() }).await.unwrap();
        drv.wait_for(keys, STEP, |d| title_of(d).starts_with(want)).await.unwrap();
    }
    stack.shutdown().await;
}
