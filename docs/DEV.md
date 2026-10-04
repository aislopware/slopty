# Development

The repository, the commands and the loop. Rules of the game are in `CLAUDE.md`; the map is
`docs/ARCHITECTURE.md`; rulings and evidence are under `docs/decisions/`.

## Layout
`crates/*` libraries, `apps/*` binaries, `xtask/` automation, `vendor/ghostty` the libghostty-vt
source, `docs/` design + decisions. GPUI comes from `aislopware/gpui-fast`, gpui-kit from
`aislopware/gpui-kit` and libghostty-vt from `aislopware/libghostty-rs`, as rev-pinned git
dependencies. `vendor/ghostty` is a submodule on our fork `aislopware/ghostty`, not on
ghostty-org's repository, and libghostty-rs pins the same commit (`GHOSTTY_COMMIT`). Each fork
carries our commits on its default branch: gpui-fast merges longbridge's branch in, the other
three are rebased onto theirs. gpui-fast is GPUI imported flat out of zed, and the fork imports
zed itself so it is never behind zed while longbridge lags.

## Dev loop
- Before coding, bring the ground up to date: `cargo xtask upstream check` and `sync` whatever
  is behind (the four forks and zed; ghostty is `vendor/ghostty`), `rustup update`, `cargo update -w`, and
  `cargo binstall -y <tool>` for any gate tool `cargo info <tool>` shows behind.
- `cargo xtask setup` installs tools (binstall) and initialises submodules, then runs
  `cargo xtask doctor`: it times the first launch of fresh binaries, and when `XProtect` scans
  them it names the app to switch on under System Settings → Privacy & Security → Developer
  Tools (the terminal, or the multiplexer if builds run inside one; only you can switch it). It
  also checks the free space against the floor below.
- `bacon` for the watch loop; `cargo nextest run -p <crate>` for one crate.
- Landing a change: stage exactly it (`git add <paths>`), run `cargo gate -m '<message>'`, then
  `git commit -F target/gate/COMMIT_MSG` without restaging, then `cargo xtask land`. The local
  gate takes about a minute with a warm build; it prints the next step when it passes. `land`
  first runs, under `nice`, the tests of the packages the commits change and of their
  dependents, on HEAD's tree (nextest's `land` profile; `--no-tests` skips it). It then pushes
  the commit to the `gate` branch, where CI runs every lane, and main moves to that commit only
  once all of them pass (below, "Gate"). A red run names the failed lane and tests in its
  summary: fix it in a new commit and land again; that push's run, which waits for any run in
  progress, decides. `land --wait` blocks until main moved or the run failed, and says which.
- Format with `cargo xtask fmt` (nightly rustfmt; stable `cargo fmt` produces different output).
- `cargo xtask e2e <case>` runs the live tests (`docs/TESTING.md`); `cargo xtask e2e server`
  is the one for the server, its worker link, the CLI and MCP, and takes seconds.
  `--filter '<nextest filterset>'` narrows any case to the tests it picks (one golden, one live
  scenario), and `--no-build` reruns the last build as it is, without cargo. Every case
  runs on this Mac alone: `workers` starts its second worker here, behind a relay shaped like
  the tailnet, so nothing waits on another machine.
- The UI draws under gpui-fast's view retention: a view is built again only when it was told
  of a change or something it read changed (`docs/ARCHITECTURE.md` §6, "Drawing under
  retention"). A view that shows an old state is a missing notify; `GPUI_VIEW_RETENTION=0` on the
  app turns retention off to confirm it, and a step in `workspace/tests/retained.rs` is how it
  stays fixed.
- `cargo xtask fixtures claude [--only <name>]` records the conversation fixtures from a real
  session (it uses the model and your login), and `cargo xtask fixtures claude-mod` records the
  mod's against a canned local API with no account. Both run the official Claude Code build
  that xtask fetches from npm into `target/claude/<version>/`, checked against the registry's
  sha512; `SLOPTY_CLAUDE` points at another binary of the same pinned version.
- `cargo xtask codex schema [--check]` writes the Codex app-server types
  (`crates/slopty-agent/src/codex/protocol.rs`) from the pinned build's
  `generate-json-schema --experimental`, and `cargo xtask codex fixtures` records
  `crates/slopty-agent/tests/fixtures/codex/` from that build's app-server against a canned
  Responses API, with nothing signed in and nothing fetched. The build comes from npm into
  `target/codex/<version>/`, checked against the registry's sha512; `SLOPTY_CODEX` points at
  another binary of the same pinned version. A Codex bump is the version in `xtask/src/codex.rs`,
  then both commands, and the diff is the wire change.
- `cargo xtask pi fixtures` records `crates/slopty-agent/tests/fixtures/pi/` from the pinned pi
  driven over RPC with Slopty's gate, against a canned Messages API, with nothing signed in and
  nothing fetched by pi. The package comes from npm into `target/pi/<version>/` by `bun
  install`, checked against the registry's sha512, and runs under `node`; `SLOPTY_PI` points at
  another `pi` of the same pinned version. A pi bump is the version in `xtask/src/pi.rs` and
  `slopty_agent::pi::VERSION`, then the command.
- `cargo xtask linux` cross-builds the terminal-only Linux worker (`slopty-ptyd`,
  `slopty-worker`, `slopty`) for `aarch64-unknown-linux-gnu` under `target/linux`, with
  `cargo zigbuild` (`cargo binstall cargo-zigbuild`; zig is the one libghostty-vt takes).
  `cargo xtask linux run` starts it in a fresh Debian container on Docker Desktop (the
  `desktop-linux` context) and prints the loopback address to dial, such as
  `slopty ping --worker 127.0.0.1:<port>`; Ctrl-C removes the container. `cargo xtask linux e2e`
  runs the Linux end-to-end test against it (`docs/TESTING.md`). The daemons' logs go under
  `target/logs/linux/<container>/`. `cargo xtask linux dist` cross-builds what ships (below,
  "Install and release").
- `cargo xtask run worker|app` to launch; `cargo xtask ios sim [--sim ipad]|device` for the phone/tablet;
  `cargo xtask bundle` builds a signed `Slopty.app` under `target/bundle` (below, "Install and
  release"), its dSYMs beside it in `dSYMs/<UUID>/`, with the icon rendered from
  `assets/icon.svg` (`cargo xtask icon` previews it);
  `cargo xtask symbolicate <report.json> [--dsyms <dir or .tar.gz>]` resolves a crash report
  from a shipped build to files, lines and inlined frames against the dSYMs of its UUID;
  `cargo xtask ime [id]` switches the macOS input source for input-method tests.
- `cargo xtask sign` gives the dev daemons a Developer ID signature under their LaunchAgent
  identifiers, so one approval of Screen Recording and Accessibility survives every later build
  (`run worker` does it for you). Without it a rebuilt daemon is a new executable to TCC and
  loses both, which surfaces as ScreenCaptureKit `-3801` and no prompt (`docs/decisions/input.md`).
- `cargo xtask upstream check` shows how far the gpui-fast, gpui-kit, ghostty and libghostty-rs
  forks are behind upstream, and how far zed is ahead of gpui-fast's import (bases in `xtask/upstream.toml`).
  For zed it reads `zed_commit` from the fork's `UPSTREAM`, counts the zed commits since that touch
  the tracked directories, and lists what an import would ask for by hand: crates joining or
  leaving the tracked set, and redirected files zed changed. Once a source's `check_every_days`
  has run out (none for gpui-fast and gpui-kit, which land changes most days, so every gate asks;
  a day for ghostty; a week for zed and libghostty-rs), the gate asks the upstream for its head
  (`git ls-remote`) and warns when it moved. A source with `paths` (ghostty) warns only when
  GitHub's compare says the move changed a file under them. For ghostty, `check` also lists the
  upstream commits since the base that touch those paths (the terminal, its C API and headers,
  SIMD, Unicode, the key and mouse encoders, the lib-vt build, `build.zig.zon`), and where
  `vendor/ghostty` stands against the fork head and the commit this repository records.
- `cargo xtask upstream watch [--interval 300] [--once]` prints a line whenever a watched
  upstream's default branch moves (its subject and how many commits it is past our base) or a
  pull request there is opened, updated, merged, closed or reopened. It watches the forks'
  upstreams and the vendored noq and objc2 (`xtask/upstream.toml`), and never syncs. For ghostty
  it names a head move only when the commits since the last look touch its `paths` (the line
  lists the files), and a pull request only when it does. What it saw
  is kept in `target/upstream-watch/state.json`, so a restart does not announce it again, and
  an upstream that does not answer is skipped for the round.
- `cargo xtask upstream sync [--only <fork>]` works in the checkouts under `.research/` in the
  main checkout. It rebases gpui-kit and libghostty-rs and merges longbridge's branch into
  gpui-fast. Then it imports zed into gpui-fast by the procedure in gpui-fast's
  `docs/upstream-sync.md`: a vendor commit `zed: import <short>` on the last one (`import_commit`)
  holding zed's tracked directories byte for byte, built from the zed checkout (`.research/zed-main`,
  fetched, its work tree untouched), merged into the fork with `UPSTREAM` rewritten. It then runs
  `script/check-upstream` and the build checks, pushes (`SSH_AUTH_SOCK` on the signing agent
  first) and moves the `Cargo.lock` pins. A conflict that is not `Cargo.lock` stops it. So does an
  import that needs a hand: a conflict, a file zed changed that a `#[path = "fast/…"]` redirect
  replaces (port it into our copy), or a crate the workspace manifest must add or drop. It stops
  mid-merge with `UPSTREAM` already staged and prints the list; finish there, `git commit`, and
  run `sync` again. `--only zed` is `--only gpui-fast`. Then gate, e2e app + ios, and a
  DECISIONS entry.
- ghostty goes through the same `sync` (`--only ghostty` brings libghostty-rs with it). It never
  rebases in `vendor/ghostty`, which every build compiles: the fork's `main` is checked out in
  the worktree `.research/ghostty` and rebased onto ghostty-org's `main` there, so a conflict
  stops the sync with the file list and `vendor/ghostty`, both forks and every pin untouched;
  resolve it in that worktree, `git rebase --continue`, and run `sync` again. Everything is
  then built and checked before anything is pushed: libghostty-rs is rebased onto its upstream,
  its `GHOSTTY_COMMIT` moves to the rebased head, the bindings are regenerated from that tree
  (the crate's `gen-bindings`) and committed, and `cargo check` and `cargo test -p
  libghostty-vt` run with `GHOSTTY_SOURCE_DIR` on it. Only then does it publish, in the order
  the pins need and confirming each: push the ghostty fork, move `vendor/ghostty` (detached)
  to that head, push libghostty-rs, `cargo update -p libghostty-vt` and check the lock pins
  the pushed binding. Stage `vendor/ghostty` with `Cargo.lock`. `--no-push` stops after the
  checks with `vendor/ghostty` and the lock as they were. `sync --only libghostty-rs` pins
  whatever `vendor/ghostty` holds, and refuses a commit the ghostty fork does not have.

## Install and release
What a person installs, and how this tree makes it.
- **`Slopty.app` is everything for a Mac.** `cargo xtask bundle` builds it in the `dist` profile:
  the app, `slopty-worker`, `slopty-ptyd`, `slopty-server` and the `slopty` CLI in
  `Contents/MacOS`, and in `Contents/Resources/workers/linux-arm64` and `linux-x86_64` the Linux
  worker (`slopty-ptyd`, `slopty-worker`, `slopty`) and server that the app installs over SSH.
  A worker or server must be this very build to let the app in, so the app carries every build it
  installs. `--no-linux` leaves the Linux builds out (no zig needed, and no Linux installs);
  `--debug` builds a dev bundle.
- **Signing.** The bundle is signed with `--sign <identity>`, else `$SLOPTY_SIGN_IDENTITY`, else
  the keychain's Developer ID Application certificate (`security find-identity -v -p
  codesigning`; here team AJ4R8GWM7A), with a secure timestamp and the hardened runtime. Each
  daemon is signed under its `LaunchAgent` label (`dev.aislopware.slopty.worker`, `.ptyd`,
  `.server`, `.cli`), so a Screen Recording or Accessibility grant made once survives every
  update, on this Mac and on each Mac a worker is deployed to. The bundle step checks that the
  worker's designated requirement names its identifier and team. With no such certificate, or
  `--ad-hoc`, it is signed ad hoc and says so: each update is then a new program to TCC and asks
  again (`docs/decisions/tooling.md`, "Bundles are signed with a stable identity").
- **The Linux builds.** `cargo xtask linux dist` cross-builds them with `cargo zigbuild` under
  `target/linux`: the worker for `aarch64` and `x86_64` against glibc 2.28 (Debian 10, RHEL 8,
  Ubuntu 20.04 and later), the server static on musl. It then runs each binary's `--version`
  on `debian:buster-slim` and the server on `alpine:3`, for both CPUs, in Docker Desktop
  (skipped, and said, when Docker is not running). The musl targets need
  `rustup target add aarch64-unknown-linux-musl x86_64-unknown-linux-musl`.
- **`cargo xtask dist`** builds a release under `target/dist-out` (`--out` elsewhere):
  `Slopty-<v>-macos-arm64.zip` (the app), `slopty-<v>-macos-arm64.tar.gz` (the CLI, the daemons
  and the server, signed as in the bundle, for a headless Mac),
  `slopty-worker-<v>-linux-<cpu>.tar.gz`, `slopty-server-<v>-linux-<cpu>.tar.gz`,
  `slopty-<v>-dSYMs.tar.gz` and `SHA256SUMS`. It checks what it made: the app's signature, every
  Mac binary arm64, every Linux one for its CPU, no worker symbol past glibc 2.28, and a static
  server. It notarises and staples the app when it is signed with a real identity and
  `notarytool` credentials are set: `SLOPTY_NOTARY_PROFILE` (a `xcrun notarytool
  store-credentials` profile), or `APPLE_API_KEY_PATH`, `APPLE_API_KEY_ID` and
  `APPLE_API_ISSUER` (an App Store Connect key). Otherwise, or with `--no-notarize`, it says why
  it skipped, and Gatekeeper asks the person to confirm the app's first open.
- **Publishing is a tag's.** `cargo xtask release` makes the version commit and the tag. Pushing
  the tag runs CI's gate on it, then the `release` job: `cargo xtask dist --out dist` on a
  hosted Mac, signed with the Developer ID in the `MACOS_CERTIFICATE` (base64 `.p12`) and
  `MACOS_CERTIFICATE_PASSWORD` secrets and notarised with the key in `APPLE_API_KEY` (base64
  `.p8`), `APPLE_API_KEY_ID` and `APPLE_API_ISSUER`. Without them it publishes ad hoc and
  un-notarised, and the run's summary says so. The archives and `SHA256SUMS` go on the GitHub
  release with the changelog's latest entry.
- **Installing.** On a Mac: unzip `Slopty.app` into `/Applications` and open it. Its first run
  offers this Mac as a worker or as the server, and a machine over SSH (a Mac, or Linux on
  arm64 or `x86_64`) as either; nothing else is to be copied by hand. Headless:
  `slopty worker install` or `slopty server install` from the Mac tarball, or from a Linux
  tarball on Linux (systemd user units; lingering is turned on so they outlive the login).
  `slopty worker deploy <ssh target>` does the same from this Mac, and registers the worker
  with the server every verb reaches (`--server`); with none to reach, the deploy says so and
  sends nothing. `slopty worker install` with no server set or found installs one beside the
  worker.

## Gate
The full gate is fmt, clippy `-D warnings` on all targets and all three Apple triples, clippy for
Linux (`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` on the server's crates and
the worker's that build there, `xtask/src/tools.rs` `LINUX_CRATES`, and
`x86_64-unknown-linux-musl` on the server's), nextest,
doctests, rustdoc, deny, hakari, shear, typos, taplo and `committed`. It is split in two:
- Here, `cargo gate` runs the lanes that take seconds to a minute: the tools lane (fmt with
  nightly rustfmt, taplo, deny, hakari, shear, typos, and `committed` on the history since the
  last tag) and host clippy. `-m '<message>'` has `committed` check the message of the commit
  about to be made too, and leaves it in `target/gate/COMMIT_MSG` for `git commit -F`. It ends
  by printing the next step.
- On GitHub Actions, every lane runs on each push to the `gate` branch, which `cargo xtask land`
  makes: the commits on main not yet on `origin/main`, pushed with a lease. The tests lane runs
  as three jobs, one per shard of packages (`--lane tests --shard ui|worker|rest`, the table in
  `xtask/src/gate.rs`), rustdoc runs after host clippy on its runner, and the tools lane runs on
  Linux, which keeps the run within five Macs. Runs on that branch
  form one concurrency group. A newer push waits behind the run in progress rather than
  cancelling it, and GitHub keeps only the newest push pending, whose green covers every commit
  under it; a pull request's run is still cancelled by its next push. When every lane passed,
  the `promote` job fast-forwards main to that exact commit (`git push origin <sha>:main`); it
  refuses one that is not on top of main, and main is never gated a second time. A failed lane
  writes the run's summary: the lane, the step that failed and, for the tests, each failed test
  from the JUnit report.

`cargo gate --full` runs every lane here, as `cargo xtask release` once did; the release now runs
the quick gate and CI gates its tag in full before it builds anything. The gate checks the
**index**, not the working tree: the staged blobs are synced into `target/gate/tree`
(submodules checked out at the commit the index pins, under `target/gate/modules`) and checked
in parallel lanes on `target/gate/*` target dirs. Several agents edit this one checkout at
once, so stage exactly the change you mean to land (`git add <paths>`), gate it, and commit it;
the tree stays free to edit meanwhile. `--fix` runs the fixers on the tree first (stage what
they changed); `--in-place` checks the tree itself; `--lane <name>` (repeat for several:
`tools`, which carries fmt, `clippy-host`, `clippy-ios`, `tests`, `rustdoc`, `linux`) runs only
those lanes, and a lane that runs alone takes every core. The `linux` lane (the Linux worker
built natively and its crates' tests) runs only on a Linux host, CI's Linux runner; here
`cargo xtask linux e2e` runs that worker in Docker instead. Per-lane times are in the log.

fmt and the tool checks (deny, hakari, shear, typos, taplo, `committed`) take seconds, so they
run first, side by side, and a failure among them ends the gate before any compile. Then the
compile lanes run. In the tests lane, nextest and the doctests run side by side once the test
binaries are built, since cargo holds its lock only while it builds.

A lane that passed leaves a record of its inputs under `target/gate/pass/`: the index entries it
reads (`mode sha path`, submodules at their pinned commit), the toolchain (`rustc -vV`), the
xtask binary, the environment cargo and the tests read (values hashed), the macOS and Xcode
builds, and the tools it runs (for the tools lane also `HEAD`, the last tag and the day, so
`cargo deny` fetches advisories daily). When a lane's inputs match its record it is skipped
("inputs unchanged since it last passed"), so rerunning a gate that did not change costs
seconds. The cargo lanes leave out paths nothing cargo builds or tests reads (`docs/`, the root
`*.md`, the tools' own configs, `xtask/src/gate/pass.rs` `inert`), so a change to the docs
alone skips them; an xtask test fails if a crate starts reading one.
`cargo gate --since-pass` narrows the tests further: when only files inside packages changed
since the tests lane last passed, nextest runs the tests of those packages, of every package
that depends on them (from `cargo metadata`, dev-dependencies included) and xtask's. A changed
file outside the packages (a manifest at the root, `Cargo.lock`, cargo's config, a vendored
tree) runs them all. Clippy and rustdoc need no flag for this, because cargo already rebuilds
only the changed crates and their dependents. Either way the gate checks the snapshot of the
index, and a skipped lane or test has already passed on the same inputs.

The gate's nextest profile is `gate` (`.config/nextest.toml`). It retries the tests named
there as timing-sensitive, which have failed under the gate's load and pass alone, up to twice.
A pass on a retry shows as FLAKY in the log. Only a named test gets retries, never a pattern.
The `ci` profile inherits `gate` (the same named retries and no others) and adds a runner's
longer timeouts and a JUnit report.

CI (`.github/workflows/ci.yml`) is the full gate, one lane per hosted runner: a matrix job per
lane runs `cargo xtask setup --lane <lane>` (only that lane's tools) and
`cargo xtask gate --ci --lane <lane>`. `--ci` checks in place, since the runner's tree is the
commit, and gives the tests lane the `ci` profile. That profile's `default-filter` leaves out
the tests that read hardware a runner's virtual Mac lacks, each named with the reason; nextest
lists them as skipped and every local gate still runs them. Only missing hardware puts a test
there, never timing and never a pattern. Compiled units come from sccache on the Actions cache
(`SCCACHE_GHA_ENABLED`); `rust-cache` keeps the registry, git checkouts and installed tools
per lane, failures included. The job's post step prints sccache's hit rate.

`cargo xtask check -p <crate>…` runs the same steps on named crates only, on the working tree:
what an agent that owns those crates runs before it reports. Its builds name `workspace-hack`
beside the crates, so every crate set resolves the third-party dependencies with the features
the gate gives them and reuses one build of each; a bare `cargo nextest run -p <crate>`
resolves its own and builds its own copies (add `-p workspace-hack` to share).
`cargo hakari generate` rewrites the hack when a manifest changes its dependencies
(`cargo gate --fix` runs it; the gate fails on a stale one).

## Disk
`target/` would grow without end: cargo never deletes a unit, and every `cargo update`, fork
rebase, toolchain or new feature set leaves the old ones behind. `cargo xtask prune` deletes, in
every target dir under `target/` and in both of cargo's layouts (to 1.99 and from 1.100):
- the units no build has read for a day, with their artifacts;
- the incremental caches no compile has touched for a day;
- the object files a binary's earlier compiles left beside it.

Then, when `target/` is over its budget (160 GB, `SLOPTY_TARGET_BUDGET_GB` or `--budget-gb`) or
its volume has less than the floor free (50 GB, `SLOPTY_DISK_FLOOR_GB` or `--floor-gb`), it
deletes the least recently used units and caches until both hold with a tenth to spare. Nothing
used in the last hour goes that way. It runs after every `check` and `gate`, skipping a dir whose
build lock is held; by hand it waits for the lock (`--idle-hours N`, `--dry-run`). It prints
what went, the largest entries and how much was used how recently. The gate will not start
under the floor: it prunes first, and if that is not enough it names what takes the space. How
prune knows a unit is in use: `docs/decisions/tooling.md`, "target/ stays under a byte budget".

A test binary runs slowly from a large directory: every process that opens `VideoToolbox` or
`CoreAudio` pays for the entries beside its executable, and `deps/` holds tens of thousands
(`docs/decisions/tooling.md`, "A test binary's directory, not the volume"). The gate's tests lane
and `cargo xtask check` therefore run each test binary through `cargo xtask test-runner`,
which runs it from a hard link in `<profile>/run/`. A bare `cargo nextest run` does not do this.
To get it, set `CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER="$PWD/target/xtask/debug/xtask-runner
test-runner"`.

## Live lane in a VM
macOS guests for the live tests that may not run on this Mac (`docs/TESTING.md` ▸ "Live lane
in a VM"). They run under tart (`brew install openai/tools/tart`). Everything lives under
`SLOPTY_VM_HOME` when it is set, else `/Volumes/Lacie/vms`, and the command refuses a home on the
startup disk (any volume of its APFS container, symlinks followed).
`--macos 26|27` picks the guest (26 by default).
- `cargo xtask vm create`: pull the pinned image (33 GB on disk for 26) and make the base
  `slopty-26`: 4 cores, 8 GB, this lane's SSH key, the worker's TCC grants, fish, no Spotlight
  or software update. The pulled manifest is hashed against the pinned digest. `--fresh` makes it again. Nothing boots the base after this; every guest is
  a copy-on-write clone of it.
- `cargo xtask vm start|stop`: the long-lived `slopty-26-dev` guest, headless (`--name` for
  another). `cargo xtask vm ssh [-- cmd]` reaches it over SSH. `cargo xtask vm exec -- cmd` runs
  a command in its logged-in session, where events post and the grants hold.
- `cargo xtask vm deploy`: build `slopty-ptyd`, `slopty-worker` and `slopty` (from cargo's own
  target dir), clone the guest if it does not exist yet, and run `slopty worker deploy` against
  it. This Mac's address is added to the guest's `[worker] allow`, other settings kept. The worker
  answers at `<guest ip>:45550` (`tart ip slopty-26-dev`, with `TART_HOME=/Volumes/Lacie/vms/tart`).
  `--app` instead deploys as the app does, over the system `ssh`, into a fresh clone of the base
  (one that never had a worker): it runs `slopty-deploy`'s `guest` test, which meets the guest's
  unknown host key, trusts the fingerprint shown, installs, and pings the worker. `--keep` leaves
  the guest. The QUIC leg needs the Local Network grant for the app the terminal runs under.
- `cargo xtask vm live -p <crate> [--test <target>] -- <nextest filters>`: a fresh guest per run,
  `slopty-26-run-<pid>-<time>`, deleted when the run ends or on Ctrl-C, SIGTERM or SIGHUP.
  `--keep` names it `slopty-26-kept-<pid>-<time>` and leaves it. The run prints the clone, boot
  and deploy times, and the results land under `target/vm/<guest>/`. Run guests take one of two
  fixed MACs, locked for the run, so DHCP leases stay at two however many runs there are.
- `cargo xtask vm e2e`: the lane's proof (a host test against the guest's worker, a real HID
  event in the guest).
- `cargo xtask vm list`.
- `cargo xtask vm prune` removes each `-run-` guest whose process is gone (a run killed outright
  leaves one; `live` and `e2e` do the same before they start), never one whose run is still going.
  `--kept` also removes the `-kept-` guests. `--all` removes every guest (base and dev included),
  the pulled images and the key, and refuses while any run's process lives.

Guests never show a window here (`--no-graphics`), and nothing in them reaches this Mac's pointer,
screen or TCC. A guest's logs are in `/Volumes/Lacie/vms/logs/<guest>.log`. macOS runs at most
two macOS guests at once. To look at a guest's screen, run `tart run <name> --vnc` by hand; no
command of the lane does.

## Deep checks (on a schedule, not per commit)
The `Deep` workflow (`.github/workflows/deep.yml`) runs these every night at 02:00 Bangkok time,
a job per check, and mutation testing on Saturdays; `gh workflow run deep.yml` starts it now
(`-f mutants=true` adds mutation testing). Each job's report is an artifact of the run
(`gh run download <id>`), and a failure opens or comments on the "Deep checks failing" issue.
Here, run one check at a time, and only the smallest that proves a change: one fuzz target for
seconds, one crate under Miri.

`cargo xtask deep <check>` runs what is too slow for the gate, each on its own target dir
under `target/deep/`:
- `miri` — the pure crates' tests under Miri through nextest's `miri` profile (nightly;
  `PROPTEST_CASES=8`, isolation off for insta; the allocation-counting binaries left out, and a
  test still running after 20 minutes ends). `-p <crate>` narrows it.
- `sanitize [address|thread]` — the tests of every crate with `unsafe` built with
  `-Zsanitizer` and `-Zbuild-std` on nightly (`SANITIZED` in `xtask/src/deep.rs`).
- `sanitize realtime` — the codec's tests under `RealtimeSanitizer`, with the audio render
  callback marked real-time (`--cfg slopty_rtsan`): an allocation, lock or blocking call
  reached from it aborts. One test proves it trips, on an allocation armed in a child process.
- `features` — `cargo hack check --each-feature` over the workspace: every feature alone,
  none, and all.
- `coverage [--html]` — `cargo llvm-cov nextest` line coverage per crate (the live e2e crate
  left out).
- `mutants -p <crate> [--timeout s] [--shard k/n] [--jobs n]` — `cargo mutants` on one crate,
  or its `k`th of `n` shares counting from 0, into `target/deep/mutants/<crate>[-<k>of<n>]`; the
  missed mutants (`mutants.out/missed.txt`) are the lines no test would notice changing.
- `fuzz [--time s]` — every fuzz target for 30 s, or `--time` (`cargo xtask fuzz` below).
- `loom` — the audio ring on loom's atomics (`--cfg slopty_loom`), every interleaving of its
  scenarios checked.
- `leaks` — the daemon and wire test binaries under `leaks --atExit`: a leaked allocation fails.
- `metal [--filter …]` — the app self-test with Metal API and shader validation on.

`cargo xtask fuzz [<target>] [--time s] [--jobs n]` builds `fuzz/` with cargo-fuzz (nightly,
AddressSanitizer, debug assertions; `cargo binstall cargo-fuzz`) and runs each target, or the one
named, for `--time` seconds (60 by default) under `nice`, in libFuzzer's fork mode so a crash is
written and the run goes on. libghostty is built `ReleaseSafe` for it, a target's dictionary in
`fuzz/dicts/` is passed when there is one, and the terminal target may hold 8 GiB, since under
AddressSanitizer its resident memory grows with every run. It replays the kept regression inputs first. The corpus of each
target grows under `target/fuzz/corpus/<target>`. Beside it, seeds are rewritten every run from
the wire goldens in `crates/slopty-proto/tests/snapshots`, and the terminal target's from the
captures in `fuzz/seeds/terminal`. Anything found lands under
`target/fuzz/artifacts/<target>/`, with the log in `target/fuzz/logs/`, and fails the run.
`--keep <artifact>` minimises one into `fuzz/regressions/<target>/`, which the fuzz crate's
`tests/regressions.rs` replays on a plain build: `cargo xtask fuzz --replay`, no nightly needed.
A new target is a function in `fuzz/src`, an entry in its `TARGETS`, a file in
`fuzz/fuzz_targets` and a `[[bin]]`; that test fails when the three disagree. The crate has its
own `Cargo.lock`, so `cargo update --manifest-path fuzz/Cargo.toml` moves its dependencies, and
the gate checks its formatting.

`cargo xtask profile -- <command…>` records a CPU profile of any command with samply into
`target/profile/<epoch>.json.gz`; `samply load <file>` opens it in the Firefox Profiler.

## Budgets, the bench, the soak and the nightly
- The allocation budgets are ordinary tests (`tests/allocs.rs` in `slopty-media`,
  `slopty-engine` and `slopty-grid`), so `cargo gate` runs them. A broken one prints what the
  path allocated now; a lower number is lowered in the test, a higher one is a finding.
- `cargo xtask bench [--filter <name>] [--update-budgets] [--wall]` runs every `*_cost`
  measurement of the crates that take `slopty-testkit` as a dev-dependency, in release, and holds
  each series' retired instructions per operation to `xtask/budgets.toml` (5 % slack). It fails
  on a series over budget, one with no budget, and a budget nothing measured. After a change
  that makes a path cheaper, or a new measurement, `--update-budgets` records the run, and the
  diff of `xtask/budgets.toml` goes in the commit with the change. `--wall` appends the wall
  times to `target/nightly/bench.jsonl` and prints how they moved since the last run there.
  The printed table carries the rows for `docs/MEASUREMENTS.md`.
- A new measurement is an `#[ignore]`d test named `*_cost` that times its samples with
  `slopty_testkit::bench::Bench` (one `series` per thing timed, `report()` at the end). A series
  whose samples run other threads is `wall_only()`: the instruction count is the process's.
- `cargo xtask soak [--seconds 1200] [--interval 2] [--stacks] [--debug] [--out <dir>]` starts the
  server, ptyd and worker from a temporary HOME and drives open, flood, hook, read and close
  cycles through the CLI. It first runs 1 536 cycles, four at a time, so the bounded stores
  (the server's event log, the worker's idempotency ledger) are full before the baseline, and
  the slope (Theil–Sen, the median of the pairwise slopes, so allocator steps and spikes do not
  move it) is taken over the load after its first 300 s and judged only once that spans 900 s
  (a shorter `--seconds` prints it as not judged). It fails on footprint growth, a peak over budget,
  descriptors or threads left behind, or a leak `leaks` finds. The samples, logs, `leaks` reports and
  `summary.json` go to `target/deep/soak/last`. The daemons it runs are copies under
  `target/deep/soak/bin`, signed ad hoc for `leaks`; the build's own binaries keep their
  signature. `--stacks` adds `MallocStackLogging`, so the reports show where a leak came from.
- `cargo xtask nightly [run] [--only <check>] [--skip <check>] [--soak-minutes 20]
  [--proptest-cases 4096] [--iterations 50]` runs the heavy lanes one after another: `soak`,
  `bench` (with `--wall`), `proptest`, `gpui-iterations`, `app-soak` (every tile kind opened and
  closed 1000 times), `miri`, `sanitize-address`, `sanitize-thread`, `sanitize-realtime`,
  `coverage`, `features`, `fuzz`, `loom`, `leaks` and `metal`. Each writes `<check>.log` and
  `<check>.json` under `target/nightly/<date>/`, beside a `summary.json`. A check whose tool is
  missing is skipped and says why. Here each check runs at `nice -n 19` with four build jobs and
  four test threads, and a full run is refused unless `--all-here` asks for it: the `Deep`
  workflow runs them (above), and `--only <check>` runs one here. On a runner the checks keep
  every core at `nice -n 10`. `cargo xtask nightly uninstall` removes the LaunchAgent an earlier
  version installed. A failing seed of `gpui-iterations` replays with
  `SEED=<n> cargo nextest run -p slopty-ui <test>`.

## Tailnet fixture
A real tailnet on loopback for the live tests that read Tailscale (`docs/decisions/testing.md` ▸
**The tailnet's live tests run against real Go daemons that xtask drives on loopback**). It needs
Go (`brew install go`) the first time, to build the pinned `tailscaled`.
- `cargo xtask tailnet up`: fetch Headscale and build `tailscale`/`tailscaled` into
  `target/tailnet/tools` on first use, then start Headscale and the nodes `worker` and `ci` under
  `target/tailnet/run`. It prints the fixture once the grant has reached the worker, and holds it
  until Ctrl-C or `down`. Run it in a second terminal, or in the background.
- `cargo xtask tailnet status`: `SLOPTY_TAILNET_FIXTURE=<path>`, then one `key=value` line for
  Headscale and one per node (`socket=`, `ips=`, `tags=`, `dns=`). It fails when nothing is up.
- `cargo xtask tailnet down`: stop `up`, then any process left under the run dir, and remove it.
- The tests: `SLOPTY_TAILNET_FIXTURE=$PWD/target/tailnet/run/fixture.json cargo nextest run -p
  slopty-tailnet --test fixture`. Without the variable they skip.
- `cargo xtask tailnet test [nextest args]`: all of it in one go (up, the tests, down), as the
  nightly's `tailnet` check runs it.
