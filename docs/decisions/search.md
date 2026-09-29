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
  - The files field takes ripgrep's globs (`*.rs`, `!tests/**`). They narrow what the ignore
    files let through, as VS Code's "files to include" does, rather than outrank them as
    `rg -g` does: given to the walk as overrides, `*.rs` brought a gitignored Rust file back
    (caught by `search_in_files_through_the_cli_and_mcp`), so they filter the walk's entries
    instead (`slopty_worker::search::walker`).
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

- ✅ **Context lines come from the searcher, each line once** (2026-09-29). A toggle beside
  the regex one asks for two lines each side of a match (`SearchQuery::context`, at most five,
  `MAX_CONTEXT`). `grep-searcher` hands them to its sink's `context`. It already merges the
  context of two matches close together and never reports a matching line as context, so
  `FileHits::context` holds each line once and the client only interleaves by line number
  (`SearchResults::rows`). A match the cap cuts off takes its context with it. The rows are
  drawn in `text_muted` with a dimmer number; ↑/↓ skip them, and a click opens the file at that
  line. No separator marks a gap between two runs of context: the line numbers already say it,
  and a separator would cost a whole row in a list of fixed-height rows.

- ✅ **Replace names what the search showed, and the worker refuses a file that moved**
  (2026-09-29). VS Code and Zed replace what their result list shows, and so does Slopty:
  - `SearchRequest::Replace` carries the search's query, the replacement and, per file, each
    match as its line and its index on the line (`MatchAt`), with the file's `FileStamp` (size
    and modification time in nanoseconds, taken before the file was read). A capped search
    replaces only the matches it showed; searching again finds the rest.
  - The worker compares the stamp before and after it reads the file whole. A file written in
    between, or gone, is `SkipReason::Changed`: its lines may have moved, and a match found by
    number could be another one. The modification time moves on every write on APFS, so no
    hash of the content is needed.
  - It finds the line's matches again with the same matcher and counts them as a hit does (not
    empty, 64 at most). `$1`, `${name}` and `$$` expand through `grep-matcher`'s own
    `interpolate` when the query is a regular expression; a literal query's `$1` is text. The
    file is rewritten through `slopty_platform::fs::replace`: a temporary file beside it,
    renamed over it, its permissions kept. CRLF endings and a missing final newline come
    through as they were.
  - `SearchEvent::Replaced` lists the files rewritten with their new stamps, and the files
    skipped with why. The client takes the replaced lines out, keeps the new stamp for the next
    replace in the same file, and moves the lines under a replacement that holds newlines.
  - An open file tile needs nothing more: its watcher compares the same size and time by path
    (`slopty_worker::file::stamp`), so the rename shows on its next look.

  The replace field sits under the query. While it holds text or the keyboard, each match reads
  struck through in the error tone, with its replacement after it in the success tone
  (`slopty_client::search::Preview`; a regular expression's groups are expanded on the client
  with the `regex` crate, which reads the same syntax). ↩ in the field replaces the selected
  line's matches (on a file row, the file's) and the selection moves on to the next match. ⌘↩ or
  the "Replace all" button replaces every match once the search is through, and the selected
  row's own button replaces that row. One replace is in flight at a time. The foot says how
  many matches in how many files, and how many files were skipped as changed since the search.
  The surface replaces a line at a time: the wire names single matches, but a row holds a line,
  and two matches of one query on one line are rare enough not to split the row.

- ✅ **Scripts and agents search through one verb** (2026-09-29). `Verb::Search { worker, root,
  query, max_lines }` answers `Outcome::Search { files, summary }`: the same search, gathered to
  its end and sorted by path (`slopty_worker::search::collect`), in one reply. `max_lines` is 200
  unless asked, what a model can read in one go, and never more than the surface's 2 000, which a
  16 MiB frame holds with room to spare even with five lines of context. `capped` says there
  were more; a narrower glob or pattern is the way on, as in the surface, so the verb needs no
  paging. `slopty search <pattern> [root] [--worker] [-g glob] [--regex] [-s] [-w] [-C n]
  [--max n]` prints ripgrep's heading format (a match's number followed by `:`, a context
  line's by `-`), or the tool's JSON with `--json`; the MCP tool is `search_files`. Replace is
  not a verb: an agent edits files with its own tools, and a replace across files belongs where
  a person reviews it.

- ✅ **A replace names the match it was shown, never follows a link, and an orchestration
  search ends with its request** (2026-09-29). Three faults a review found in the batch above:
  - A line longer than 240 bytes was cut to start at its indentation, so matches inside the
    indentation (a search for a space or a tab) were left out of the spans. The client names a
    match by its place among the spans, and the replace counts every match from the line's
    start, so replacing the first match shown rewrote an indentation space. The cut now never
    starts past the line's first match (`line_hit`). Test:
    `a_replace_on_a_long_indented_line_rewrites_the_match_shown`.
  - The replace looked at the file with `symlink_metadata`, then opened it and wrote through
    `slopty_platform::fs::replace`, both of which follow links, so a link swapped in between (or
    a directory below the root swapped for a link) sent the write out of the folder. Each
    directory on the way is now opened from the last with `O_NOFOLLOW`, the file too, and the
    new contents are written beside it and renamed over it inside the directory held open, after
    checking the name still holds the file that was read. Test:
    `a_link_swapped_in_after_the_search_is_not_written_through`.
  - `Verb::Search` walked with a stop flag nothing set, so a sparse search of `~` or `/` ran to
    the end of the tree after its caller had given up. The flag is now set when the verb's future
    is dropped (`search::StopOnDrop`), and the walk stops after 30 s (`SEARCH_WITHIN`, under the
    server's 60 s) with what it found, capped. Tests: `dropping_a_search_stops_its_walk` (the
    dropped walk ended 0.2 ms after its drop, a whole one takes 150–215 ms),
    `a_collected_search_stops_when_told_or_late`.
