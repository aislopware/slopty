# Decisions — Search in files

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **The worker searches with ripgrep's own libraries, and streams what it finds**
  (2026-09-29). VS Code Remote and Zed run a project search on the remote machine, next to
  the files, and so does Slopty: `ClientMsg::Search(SearchRequest::Start { id, root, query })`
  goes to the worker, which walks the tree with `ignore` (already a dependency for quick open)
  and reads each file with `grep-searcher` through a `grep-regex` matcher. Those are the crates
  ripgrep itself is built from, pure Rust, so the rules a developer already knows hold as they
  do in `rg`:
  - `.gitignore`, `.ignore` and the global excludes are honoured, in a git repository or not.
  - Hidden files are searched, since a `.github` workflow is part of a project, but `.git`,
    `.hg`, `.jj` and `.svn` are not.
  - A file with a NUL byte is binary and reports nothing. Its lines are gathered before any
    are sent, so a file found binary half way leaves no trace.
  - The files field takes ripgrep's globs (`*.rs`, `!tests/**`).
  - The toggles are `grep-regex`'s own: case (`case_insensitive`), whole word (`word`) and
    regular expression (`fixed_strings` when off).

  The walk runs on half the machine's cores, two to eight, because the worker streams
  terminals and windows while it searches. Over 3 002 files on this Mac's internal disk the
  whole round trip took 48 ms (`a_text_search_streams_its_matches_and_a_new_one_stops_the_last`
  in `apps/slopty-worker/tests/e2e.rs`, `--nocapture`).

- ✅ **Pages on the control stream, bounded, capped and numbered** (2026-09-29). Matches
  come back as `WorkerMsg::Search(SearchEvent::Hits { id, files })`:
  - A page goes out when it holds 32 KiB of line text (`PAGE_BYTES`) or when its first file
    has waited 16 ms, so the first match shows while the walk is still going.
  - A line is cut to 240 bytes round its first match (`LINE_BYTES`), because a minified bundle
    is one line megabytes long.
  - A search stops at 2 000 matching lines (`MAX_LINES`), and `Done` says so. A search for `e`
    in a home directory is then a few hundred kilobytes, not the disk.

  A search's traffic is small and bursty. A uni stream of its own would need a new `UniHead`
  and its plumbing in `slopty-net`, and would buy nothing at these sizes: the terminal rows
  and their echoes do not ride the control stream, and a page is no bigger than a folder
  listing. Each event names its search, so the client drops a page still in flight for the
  query before (`slopty_client::search::SearchResults::apply`).

- ✅ **One search per connection; a new one stops the last** (2026-09-29).
  - `slopty_worker::search::Searches` holds the search in progress as a cancel flag, which
    the walk's threads, the searcher's sink and the pager all read.
  - A new `Start` sets that flag, and so do `Stop { id }` and the connection ending.
  - A stopped search sends nothing more, not even its end, and a page it has queued waits on
    the connection's bounded queue, so a slow client holds the walk back rather than its
    memory.

  The surface searches as the field is typed, 120 ms after the last key (`DEBOUNCE`), which
  is VS Code's search-on-type. Each change starts a new search; the stop is implied.

- ✅ **The surface floats over the workspace like the palette, and is kept when it closes**
  (2026-09-29). ⌥⌘F (⌘⇧F was already "Find in every tile") or the palette's "Search in files"
  opens `slopty_ui::search::ProjectSearch` over the strip.
  - It sits on the palette's anchor, at the editor overlay's width, since its rows are lines
    of code.
  - The query is set at the title size, with the three toggles beside it (⌥⌘C, ⌥⌘W, ⌥⌘R,
    VS Code's keys). The files field and where the search looks are under it.
  - The results are grouped by file in path order, whatever order the worker's threads find
    them in, so a search reads the same every time. A file's row shows its name, its folder
    and its count, and a click folds it. A line's row shows its number and its text, each
    match tinted as a file tile's find tints its hits.
  - The selection follows the first match until a key or the pointer moves it, so ↩ opens
    the top result as the pages arrive.
  - A match opens its file tile through `open_file_on` at its line.

  It searches what the focused tile is about: a shell's repository, else its directory; for
  a file tile, the repository of a shell it sits under, else its folder; a folder tile's
  folder. Failing those it takes the shell a "run" would go to, then the worker's home.

  It is an overlay rather than a tile because a tile is an item in every client's shared
  document, and a search is one person's question. Closing it stops a search still going. The
  workspace keeps the surface, so ⌥⌘F brings the same query and results back, and runs a
  stopped search again.

- ⏸ **Replace across files, context lines round a match, and an MCP verb for agents**
  (`slopty-tools`) are deferred.
