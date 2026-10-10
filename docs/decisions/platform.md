# Decisions — Platform

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **Floor macOS 26.5 / iOS 26.5, Apple silicon only.** User decision 2026-09-04. No
  availability checks, no fallbacks for older OS.

- ✅ **Pure Rust.** Scripts are `xtask`. The only non-Rust files are Metal shaders and the
  XcodeGen spec that xtask generates. The iOS app's delegates and `main` are Rust too
  (`objc2::define_class!` and `UIApplication::main` in `apps/slopty-ios/src/lib.rs`, 2026-09-29,
  replacing a `main.m` shim). Xcode produces no binary from a target without sources, so xtask
  hands it an empty object compiled by rustc and `-force_load`s the library that holds `main`.

- ✅ **Dock badge + bounce on macOS** (2026-09-05). `CanvasEvent::NeedsYou(n)` also calls
  `slopty_platform::set_badge(n)` (`NSApplication.dockTile.badgeLabel`, cleared at 0 and on
  every reconnect), and `Attention` adds `slopty_platform::bounce()`
  (`requestUserAttention(NSCriticalRequest)`, skipped when the app `isActive`, so a user looking
  at the canvas gets only the sound). Both need no bundle or entitlement, unlike
  `UNUserNotificationCenter`. iOS: no-ops — the icon badge needs notification authorisation and
  the connection dies in the background anyway, so the in-app "N need you" pill is the signal.
  Verified 2026-09-05 with a synthetic `PermissionRequest` through `slopty hook`: the tile
  shows "1" (bare binary and bundle alike), `PostToolUse` clears it. Gotcha: `cargo build -p
  slopty-app` builds only the library crate; the binary is `cargo build -p slopty --bin
  slopty-app`. GPUI's `show_system_notification` exists in the fork but is disabled outside a
  bundle ("system notifications disabled: not running from an app bundle") — next step for
  banners now that `cargo xtask bundle` exists.

- ✅ **Notification-centre banners for agents** (2026-09-05). `CanvasView::agent_event` posts a
  GPUI `SystemNotification` (tag = session id, title "Claude wants to use Bash" / "has a
  question" / "finished", body = the badge detail, Allow/Deny action buttons for a permission)
  when `attention` is set and `cx.active_window()` is `None` (the app is not frontmost —
  `NSApp.mainWindow` is nil then). Answering from the badge or the agent moving on dismisses it
  by tag. `cx.on_system_notification_response` (registered once in `open_workspace`) parses the
  tag, activates the app and calls `CanvasView::notification_response`: body → reveal the
  session, "allow"/"deny" → the badge's Enter/Esc. GPUI's macOS backend disables all of it
  outside a bundle, so this only works from `cargo xtask bundle` output (the first post raises
  the macOS "Slopty Notifications" prompt). Verified 2026-09-05 with the debug bundle: the
  notification landed in Notification Center ("allow? $ cargo test"), clicking it brought Slopty
  forward with that terminal active. The action buttons are wired through GPUI's category
  registration but were not exercised (macOS only shows them on hover of a live banner).
  iOS: no-op — the connection dies in the background, nothing would post.

- ✅ **Constants the SDK defines as `CFSTR` macros are spelled once, next to their use**
  (2026-09-06). "Apple framework keys and constants come from the objc2 statics, never string
  literals" is written for constants that have a symbol: the static is the guarantee that the
  spelling is the SDK's. `kAXWindowsAttribute`, `kAXUIElementDestroyedNotification` and the rest
  of `HIServices/AX*Constants.h` are `#define … CFSTR("…")` — no symbol is exported, objc2
  generates nothing for them (`AXNotificationConstants.rs` holds only `AXPriority`), and there is
  no static to come from. The carve-out: such a constant is a `const &str` in the one module that
  uses it, with a comment naming the macro and the SDK header, and nothing else in the workspace
  spells it. A binding that later exports the symbol replaces the `const`. Rejected: a
  `slopty-apple-constants` crate (a second place for a thing the SDK itself keeps in one header),
  and a runtime lookup of the header (no).

- ✅ **Linux seams: the worker's side builds for Linux, and what Linux cannot do says so**
  (2026-09-27). Workers and the server are to run on Linux as well as macOS; only macOS ships
  now, so the seams go in before the code that would otherwise grow around their absence.
  - **One crate, cfg per item.** `slopty-platform` no longer compiles to nothing off Apple.
    The client's half (pasteboard, drops, drags, the browser tile, the Dock, haptics) stays
    `cfg(target_vendor = "apple")`. The worker's and server's half builds everywhere, with a
    `linux` module beside the Apple code. No trait: each call has one implementation per
    target, chosen at compile time, and no availability check exists on either side.
  - **Done now, because it is cheap and correct.**
    - `dirs`: the data directory (`$SLOPTY_DATA_DIR`, else Application Support on macOS,
      `$XDG_DATA_HOME/slopty` or `~/.local/share/slopty` on Linux) and the socket directory
      (`$TMPDIR/slopty` on macOS; `$XDG_RUNTIME_DIR/slopty`, else `/tmp/slopty-<uid>` on
      Linux, since `/tmp` is shared). `dirs::Layout` takes the environment as an argument, so
      both rules are tested on the Mac. The server's store resolves through it.
      `slopty_pty::protocol::socket_path` spells the socket rule a second time, because ptyd
      must not link `slopty-platform` and the AppKit it carries on macOS.
    - `service::Service::systemd_unit`: the user unit a Linux install writes (restart on
      death, start at login, `After=` ptyd), with systemd's quoting of `%`, `$`, `"` and `\`.
    - `computer_name`: `scutil` on macOS, the host name on Linux. The server uses it.
    - `open_url`: `xdg-open`, reaped on its own thread.
    - `slopty_pty::process::foreground`: `/proc/<pid>` (`stat` for the name and the start in
      ticks since `btime`, `exe` to complete a `comm` cut at 15 bytes, `cmdline`, `cwd`). The
      start is whole-second `btime` plus ticks, so it reads the same every time and still tells
      a process from its pid's next owner. The PTY itself was already one rustix `openpt` path.
  - **Explicitly unsupported on Linux.** `Activity::latency_critical` holds nothing: Linux has
    no App Nap. `user_interactive_thread` changes nothing: every Linux equivalent of a QoS
    class needs `CAP_SYS_NICE`. The desktop:
    - `slopty_worker::platform::Native` is `headless::Headless` off macOS. Its capture
      enumerates and resolves to `CaptureError::Unsupported`, its encoders cannot be made
      (`CodecError::Unsupported`, `NoAudio`), and its input refuses every event
      (`InputError::Unsupported`). What a stream would hold (a target, a stream, a watch) is an
      uninhabited `Never`, so no code path can reach a value it never had. It builds on every
      target, so the Mac checks and tests it.
    - `caps::probe` sends `Os::Linux`, the distribution from `/etc/os-release`, `sysinfo`'s
      memory, and no encoder, display, capture or injection; no client offers a stream. The
      daemon's desktop start-up and input release (`desktop`, `let_go`) and the doctor's grant
      checks are macOS-only.
    - The clipboard is one the worker holds itself (see "A Linux worker holds its clipboard",
      2026-10-01). Off macOS, `board_type` spells each format as its MIME type; a Mac test holds
      the macOS spelling to AppKit's statics.
  - **Also on Linux, from `/proc`.** A session's listening ports: children from each thread's
    `task/*/children` (every `stat`'s parent where the kernel keeps no such list), socket inodes
    from `fd/*`, listeners in state `0A` from `net/tcp` and `net/tcp6`.
  - **Installed as systemd user units.** `slopty worker install` and `slopty server install`
    describe each daemon once (`slopty_platform::service::Service`) and hand it to the
    session's `Manager`: a `LaunchAgent` plist and `launchctl` on macOS, a unit in
    `~/.config/systemd/user` and `systemctl --user` on Linux, logs in the journal. Install tells
    a user without lingering to `loginctl enable-linger`, or the worker stops at logout.
    The app's, the worker's and the CLI's paths resolve through `slopty_platform::dirs`
    directly; the forwarding wrappers are gone (2026-09-28).
  - **The gate holds it.** The Linux lane lints the server's crates and the worker's with
    everything under it (`slopty-worker`, `slopty-workerd`, `slopty-platform`, `slopty-pty`,
    `slopty-ptyd`, `slopty-agent`, `slopty-engine`, `slopty-capture`, `slopty-input`,
    `slopty-codec`, `slopty-media`) for `x86_64-unknown-linux-gnu` and
    `aarch64-unknown-linux-gnu` in one pass, and the server's again for musl, how it ships.
    glibc because a Linux desktop's libraries (PipeWire, VA-API) are glibc's. aarch64 because
    its `c_char` is unsigned. blake3's NEON C needs a Linux C toolchain the Mac lacks, so that
    pass sets its `no_neon` feature; clippy never links. The CLI and `slopty-client` are on the
    lane too (below).
  - Tests, all run on the Mac: `dirs`'s `the_data_directory_follows_each_platforms_rules` and
    `sockets_go_in_a_directory_of_the_users_own`, `service`'s
    `a_service_is_written_as_a_systemd_user_unit`, `slopty_pty::process`'s
    `a_process_is_read_off_procfs`, the worker's `a_headless_platform_refuses_every_desktop_call`,
    `listeners_and_parents_are_read_off_procfs` and
    `os_release_names_the_distribution_and_version`, `slopty_input`'s
    `an_unsupported_board_holds_nothing_and_refuses_writes` and
    `the_spelled_out_type_names_are_appkits`, and the CLI's
    `a_linux_install_writes_systemd_user_units`.
- ✅ **A terminal-only Linux worker is whole: the CLI, git, the shell, and sleep** (2026-09-28).
  Without the CLI a Linux box had no `slopty worker install` and no `slopty hook`, so no agent
  status.
  - **The CLI and the client core build for Linux.** `slopty_client::screen` decodes with
    VideoToolbox, so it and `warm_up_decoder` are `cfg(target_vendor = "apple")`; off Apple the
    link keeps no screen router and drops a datagram that is not a terminal echo, since no
    screen stream is opened without a decoder. `slopty bench screen` moved to
    `bench/screen.rs` and exists on Apple only; the CLI takes `slopty-capture` there only.
    Everything else the CLI does (verbs, `mcp`, `worker`, `server`, `hook`, `attach`, `bench
    echo`) is the same code on both.
  - **git.** `slopty_worker::changes::find_git` searches `PATH`, then each host's own
    directories: on a Mac Homebrew's and the developer tools', never `/usr/bin` (the shim that
    offers to install them); on Linux `/usr/local/bin` and then `/usr/bin`, where git is the
    distribution's.
  - **The shell.** `$SHELL`, else the passwd entry's (a systemd unit has no `SHELL`, as a
    `LaunchAgent` has none), else what the system gives a new account: `/bin/zsh` on macOS,
    `/bin/bash` on Linux.
  - **Sleep.** `Activity::system_awake` takes a blocking logind `sleep:idle` inhibitor and
    `display_awake` an `idle` one, under the worker's existing policy (the machine while a
    client is attached, the display while a stream is live). The lock is `systemd-inhibit
    --mode=block … cat`, logind's own D-Bus client, with `cat` reading a pipe the hold owns.
    Dropping the hold closes the pipe; so does the worker dying, so no lock outlives it. A
    hand-written D-Bus client would mean SASL, the wire marshalling and fd passing, none of it
    testable on the Mac; `zbus` is a tree of crates for one call. With no `systemd-inhibit` the
    hold logs "sleep not held", and a lock logind refuses shows as the child's failed exit.
    `sleep:idle` rather than an idle-only lock like macOS's: `idle` alone stops logind's
    `IdleAction`, but a desktop that suspends on its own timer asks logind to `sleep`, which
    only a `sleep` lock refuses.
  - **The lane builds tests.** Every Linux crate's tests are linted for both glibc triples
    except `xtask::tools::LINUX_UNTESTED` (`slopty-capture`, `slopty-input`, `slopty-worker`,
    `slopty-workerd`), whose tests drive ScreenCaptureKit, `CGEvent` or VideoToolbox or take
    objc2 as a dev-dependency; gating those is the next step.
  - Tests, run on the Mac: `changes`'s `git_is_found_by_each_hosts_rule` and `pty`'s
    `the_shell_is_the_environments_then_the_accounts_then_the_systems`.
    `an_inhibitor_blocks_for_as_long_as_its_pipe_is_open` holds the command line and builds in
    the Linux lane; nothing here has run on Linux.

- ✅ **Linux proven in a container** (2026-09-28). The terminal-only Linux worker now runs,
  not only builds. A Debian container on this Mac's Docker Desktop hosts it, and this Mac
  reaches it the way it reaches any worker.
  - **Cross-linked here, with zig.** `cargo xtask linux` builds `slopty-ptyd`,
    `slopty-worker` and `slopty` for `aarch64-unknown-linux-gnu` with `cargo zigbuild` under
    `target/linux`. Zig is the C toolchain and the glibc linker. It is already the toolchain
    libghostty-vt builds with, and zig builds ghostty for `-Dtarget=aarch64-linux-gnu`
    unchanged. So nothing new is installed but `cargo-zigbuild` (in `cargo xtask setup`). The
    build also reuses sccache and the Mac's crate and git caches: 2.5 minutes cold, seconds
    warm. Rejected: building inside a `rust` container on the bind-mounted tree. That needs
    rustup, the pinned toolchain and zig 0.16 in an image, and a cold registry each time. It
    would also compile on the external volume through Docker Desktop's file sharing, the
    slowest disk path this Mac has. aarch64 because Docker Desktop's VM on Apple silicon is.
    x86_64 stays a clippy-only triple.
  - **Run as a Linux install runs.** `cargo xtask linux run|e2e` starts a fresh
    `debian:trixie-slim` with the binaries mounted read-only at `/opt/slopty`, under
    `--init`, capped at two CPUs. It adds an account whose passwd shell is bash, and runs ptyd
    and the worker as that account on the defaults: `~/.local/share/slopty`, and
    `/tmp/slopty-<uid>` for the sockets, since a container has no `XDG_RUNTIME_DIR`, and no
    `SHELL`. The container is named `slopty-linux-<pid>-<ms>` and labelled `dev.slopty.linux`.
    `e2e` removes it at the end and `run` on Ctrl-C (its init ends the `sleep`, and `--rm`
    removes it); the `sleep` bounds an orphan to an hour. Only the `desktop-linux` context is
    ever used.
  - **Admitted by the one address it is reached from.** A container is not on the tailnet,
    and the worker admits no private range by default. Docker Desktop forwards the published
    UDP port on this Mac's loopback into the VM, and the packets reach the container from its
    bridge's gateway (`172.17.0.1`). So the xtask writes that one address, read from
    `docker inspect`, as `[worker] allow`. That is the setting a VPN or LAN user writes; no
    test path exists in the worker.
  - **What broke on Linux.** Two things, both in the CLI:
    - `slopty hook` spelled its own default control socket, `$TMPDIR/slopty/worker.sock`, and
      so missed a Linux worker's `/tmp/slopty-<uid>/worker.sock`. A session's own relay was
      told the path by `SLOPTY_WORKER_SOCKET`; a relay started any other way stayed silent,
      since the relay never fails. The relay, `hook report` and `hook statusline` now resolve
      it through `workerctl::socket`, the one rule `slopty worker` uses, over
      `slopty_platform::dirs`.
    - `slopty worker doctor` listed Screen Recording and Accessibility as missing on Linux
      and exited non-zero. The grants are now checked only where there is a desktop to stream
      (macOS); elsewhere the report says terminals, files and agents only.
  - **Proven end to end** (`crates/slopty-e2e/tests/linux.rs`, gate `SLOPTY_LINUX_E2E`).
    - The greeting says Linux, aarch64 and `debian 13`, with no capture, input, encoder or
      display.
    - The passwd shell echoes a typed command and runs it (`uname -s` is `Linux`).
    - `ListFolder` lists a folder made in that shell, folders first, and `ReadFile` reads a
      file in it.
    - A `UserPromptSubmit` hook, played through the Linux `slopty hook` inside the container,
      finds the worker by the platform's socket rule and reaches the client as `Working` with
      the prompt.
    - 300 keys into `cat` echo in 0.7 ms at p50 and 1.4 to 2.1 ms at p99 over loopback
      through the VM (`docs/MEASUREMENTS.md`, 2026-09-28).
    - The Mac's own `slopty ping --worker 127.0.0.1:<port>` reaches it too.
  - **Not covered.** systemd: the container has no init system, so `slopty worker install`,
    lingering and the logind sleep lock (`no systemd-inhibit; sleep not held`, as designed)
    are still proven only by their Mac-side tests. A real network path, x86_64, and the tests
    `LINUX_UNTESTED` keeps off the lane are not covered either. (Since then: systemd and
    lingering by `cargo xtask linux deploy`, `workers.md`; inotify, search, uploads, tunnels and
    x86_64 below, "A Linux worker's files, search, uploads and tunnels".)

- ✅ **This Mac as a worker** (2026-09-28). The app turns the Mac it runs on into a worker, as
  `slopty worker install` does from a shell: "Use this Mac as a worker" is a quiet link under the
  first run's other way in and the add-worker dialog's, and a palette command after that.
  - **One installer.** The install moved into `slopty_platform::service`: `install_worker`,
    `uninstall_worker`, `install_server`, `Session::state` and `Session::restart`. The CLI and
    the app both call it. A `Session` carries the home, the definitions directory, the uid
    and a `Runner` for `launchctl` and `systemctl`, so the tests install into a temporary
    directory with a runner that records the command lines, and no test loads a real agent.
    Waiting for the daemon stays with the caller: the CLI asks for `status`, and the app asks
    for `doctor`. Both go over `service::ask`, one request line to the control socket and one
    reply line.
  - **Where the daemons run.** Suppose the binaries sit in an app bundle's `Contents/MacOS` on
    the volume the home is on. Then the agents run them in place. `cargo xtask bundle` already
    ships `slopty-worker`, `slopty-ptyd` and `slopty` there, signed, so a Screen Recording grant
    stays with the one signed path an update replaces. Anywhere else they are copied to
    `<data dir>/bin/` as before. A dev tree gets rebuilt under them, and a binary on an
    external volume hangs in dyld under launchd. A bundle on an external volume is copied from
    too, for the same reason.
  - **The checklist is the worker's `doctor`.** The app reads `slopty_proto::ctl::Health`,
    the control protocol's own type, so it needs no dependency on `slopty-worker`. It makes
    four lines of it: worker running, Screen Recording, Accessibility, and reachable on the
    tailnet. A missing grant's button opens its own
    Privacy & Security pane (`slopty_platform::privacy`, where each
    `x-apple.systempreferences:` URL sits next to the call that opens it). The grant goes to
    `slopty-worker`, not the app, so the line names it. The tailnet line only warns: loopback
    needs no tailnet, so it does not hold up the add.
  - **Back in front, asked again.** When the app becomes active again, a worker short of ready
    is asked again. If Screen Recording was missing, it is kickstarted first, because a running
    process does not see that grant until it starts again, and its sessions live on in ptyd.
    Once both grants read true the app adds `127.0.0.1` and closes the panel onto that worker.
  - **It joins the app's server.** Before the install, a worker with no `[worker] server` of its
    own takes the app's `[client] server` (`slopty_settings::join_clients_server`). The server's
    other clients see this Mac too, as `slopty worker install --server` would give them. This
    app keeps its loopback link; it knows the worker by id, so the directory's listing of the
    same worker is not a second one.
  - **Nothing real under test.** Every effect goes through `this_mac::Host`: install, doctor,
    restart, add and open a pane. The app tests use a stand-in. The e2e build gets no host at
    all, so the self-test can never install anything.
  - Tests: `service`'s `from_a_bundle_the_agents_run_the_bundles_binaries`,
    `outside_a_bundle_the_agents_run_copies_and_uninstall_reverses_it` and
    `a_missing_binary_installs_nothing`; `this_mac`'s `the_doctor_maps_to_lines_and_buttons`;
    the app's `this_mac_installs_then_is_added_once_its_doctor_is_green`.
  - Superseded in part 2026-10-04 by **The first Mac runs the server** (topology.md). The
    install decides the server first and starts one here when none answers; the worker
    registers with it, and `[client] server` follows the worker's (`slopty_settings::join_server`
    replaced `join_clients_server`). The checklist has five lines, a Server line after Running,
    read from the doctor's link to its server. The flow ends when the server's directory lists
    this Mac, with no loopback add (`this_mac::Host::add` is gone). `this_mac::Host::repoint`
    repoints the server's agent too. Tests: the app's
    `this_mac_runs_the_server_then_waits_for_it_to_list_the_worker` and
    `this_mac_asks_which_server_when_it_cannot_tell`; `this_mac`'s
    `the_server_line_follows_the_server_then_the_workers_link`.

- ✅ **A pocketed phone is told when an agent needs the human or a long command ends**
  (2026-09-28). A phone in a pocket saw nothing until it was opened again: GPUI's iOS platform
  posts no notification, and the connection was assumed to die in the background (the 2026-09-05
  Dock entry above). Rulings:
  - **Through `UNUserNotificationCenter`, in `slopty_platform::notify`, for both systems.** It
    uses objc2 (`objc2-user-notifications`), not the zed fork, because the fork's changes cannot
    be pushed for now. `System` is the centre. `Memory` records and shows nothing. Both sit
    behind the `Notifier` trait, so no test and no self-test run can raise a prompt or a banner.
    The self-test's window is never in front, so a real notifier there would notify on every
    agent. Outside an app bundle the centre aborts the process, so `System` keeps none there and
    drops its notes, and on macOS the Dock still shows the badge.
  - **Authorisation is asked for once, lazily.** The first note posted asks for alerts, sounds
    and the badge. Notes posted while the question is out wait, newest per identifier, and go
    out if it is granted. Making a `System` installs the delegate and reads the settings, and
    neither prompts. An app the person already allowed therefore shows its badge before its
    first note.
  - **What notifies** (`slopty_ui::workspace::attention`). An agent that starts to need the
    human while the app is not in front notifies: a permission, a question, input it asks for.
    Its title is the tile's name (the worker's for an agent with no tile here) and its body is
    `agent_ask_line`. A shell command that ran at least the slow-command time (`SLOW_COMMAND`)
    and ends while the app is away notifies too, with the command and how it ended. So does an
    agent's turn that ran that long and finished on a tile out of sight (2026-10-01): it badges
    the tile, lands under Finished in the inbox with the agent's own summary, and counts toward
    the bell, because the end of a long agent turn is what the person waits for just as much as
    the end of a build. A turn that goes idle without finishing earns nothing. An agent
    already waiting when the app left says nothing new. Nothing notifies while the app is in
    front, where the inbox says it.
  - **One note per tile.** A note's identifier is its session's, so a newer one replaces the
    older. An agent answered anywhere takes its own note back. Coming back to the app takes back
    every note it posted, because the inbox now shows the same things.
  - **The badge is the inbox's unread count**: agents waiting plus commands that finished
    unwatched. That is the Dock's badge on macOS and the icon's on iOS, where it shows once
    notes are allowed. This replaces the needs-you count of the 2026-09-05 entry, and takes back
    the terminal ruling that a finished command earns no system notification (`terminal.md`,
    "A long shell command that ends unwatched badges its item", ruling 6): on a phone in a
    pocket, the end of a build is what the person is waiting for.
  - **A tap opens the app on its tile.** The note's `userInfo` carries the worker key, the item
    and the session. `WorkspaceView::open_notification` focuses the tile (`go_to`) and gives a
    shell the keyboard. An agent with no tile here gets one. Nothing in the app uses GPUI's
    centre any more, so its delegate is never installed and ours is the only one.
  - **A program's own notification is a note too.** OSC 9 / 777 / 99 from a program in a shell
    goes through the same notifier (`Attention::program`), titled by the tile, while the app is
    away. The workspace keeps only the Dock bounce. One centre and one delegate means every tap
    routes the same way, and no banner goes out twice.
  - **Watched means in front.** A long command that ends on the focused tile while the app is
    away was not watched: it enters the inbox and the badge, as its note says
    (`a_command_ending_on_the_focused_tile_while_away_enters_the_inbox`).
  - **Background grace on iOS.** Leaving the front begins a `beginBackgroundTask`, and coming
    back ends it. The expiration handler ends it too, as UIKit requires. The links stay up for
    the time the system grants, so what arrives just after the phone is pocketed still notifies.
  - **A tap that launches the app is routed too.** The system gives the launching tap only to a
    delegate set before `application:didFinishLaunchingWithOptions:` returns (the SDK header).
    So `notify::install` sets one delegate per process at launch: from the iOS app delegate,
    because GPUI starts only when the scene connects, and on macOS from GPUI's run callback,
    which runs inside `applicationDidFinishLaunching:`. Taps wait in a process-wide queue until
    `open_workspace` listens (`notify::taps`), and one nobody heard waits for the next listener.
  - Tests: `slopty-ui` `workspace::attention::tests` (only while away, one note per tile and
    what takes it back, a long command while away, the badge as the unread count, the look's
    title and ask line from a headless workspace, a tapped note focusing its tile);
    `slopty-platform` `notify::tests::memory_keeps_what_it_was_told_in_order` and
    `a_tap_before_the_app_listens_arrives_once_it_does_exactly_once`.
  - Closed since: the workspace's own GPUI banners (`WorkspaceView::notify_system`,
    `notify_program`) are gone, so an agent's note goes out once (e07d0c3d, 2026-09-28), and
    `command_finished` counts the focused tile as watched only while the app is active
    (304adfe7, 2026-10-02).

- ✅ **One way to replace a file, one home** (2026-09-28, audit of the non-UI crates). Seven
  copies of write-temporary-then-rename (hook settings, the item registry, the transfer
  ledger, a worker file save, the server's store, the directory cache, the known workers) and
  the layout save are now `slopty_platform::fs::replace`. It writes a temporary file in the
  same directory, orders its data ahead of later writes (`F_BARRIERFSYNC` on Apple,
  `fdatasync` elsewhere), renames it over the file and syncs the directory. It keeps the old
  mode, follows a symbolic link and refuses anything but a regular file. It never uses
  `F_FULLFSYNC`: that flushes the drive's cache and buys only that the very last write
  survives a power cut, which none of these small state files needs. `settings.toml` is
  written the same way, so the app's watcher never reads half a file.
  - **One home.** `slopty_platform::dirs::home` is `$HOME`, else the passwd entry, used only
    when absolute, else `/`; never `/tmp`, which every user can write to, so a hook settings
    file or a terminfo database can't be planted there. Eight reads with three different
    fallbacks now go through it. `slopty-ptyd` keeps its own copy of the rule in
    `slopty_pty::pty::home`: linking `slopty-platform` would load AppKit and WebKit into the
    PTY custodian for its whole life.
  - Test: `fs::tests::replace_swaps_the_contents_whole_and_keeps_the_mode`,
    `dirs::tests::the_home_is_absolute_and_the_environments`.
- ✅ **No macOS names on the wire** (2026-09-28, audit finding 21).
  - **The clipboard names its formats.** `ClipFormat` is one of `FileUrls`, `Png`, `Tiff`,
    `Rtf`, `Html` or `Text`, each with a MIME type (`ClipFormat::mime`). It replaced the Apple
    UTI strings in `ClipItem`, `ClipMsg::Fetch`/`Data` and `Purpose::Clip`. The pasteboard seam
    maps it: `slopty_input::board_type` gives the AppKit type on a Mac and the MIME type
    elsewhere, and `format_of` reads a board's type back.
  - **Amended 2026-09-30: Apple types travel as UTIs.** The first version had no
    `Other(String)`, on the grounds that an unknown format has nowhere to go. Between two Apple
    ends, the common case here, it has: both pasteboards take any UTI as it is. So a
    representation is a `ClipType`, one of the formats above or `Apple(String)`, a UTI that only
    an Apple end writes and a Linux end ignores (`type_on_board` answers `None`). The sender
    drops what means nothing elsewhere (`transfer::carried`). See Audio, **Clipboard v2**.
  - **A display is a `DisplayId`**, a newtype in `slopty-core`, not a bare `u32` that happened
    to be a CoreGraphics id. `server::DisplayCap` duplicated `screen::DisplayInfo` and is gone,
    so `WorkerCaps.displays` carries `DisplayInfo`.
  - **`Os` has no default.** A worker says what it runs on. `WorkerCaps::bare(os)` is the
    fixture constructor.
  - **The shell fallback is `/bin/sh`** when `SHELL` is unset, not `/bin/zsh`, which a Linux
    worker may not have.
  - Goldens: `client_clip_offer`, `worker_clip_data` and `worker_clip_fetch`. Tests:
    `each_format_is_an_apple_uti_and_back` (`slopty-input`).
- ✅ **Paste on iPhone without the prompt: the system's paste button** (2026-09-29, product
  gaps C7). iOS asks before an app reads another app's clipboard, unless a paste the person
  made does the reading. `UIPasteControl` is that paste: UIKit draws it, knows the tap was the
  person's, and hands its target the clipboard's item providers with no prompt.
  - **The platform half.** `slopty_platform::paste_control::PasteButton` puts the control in
    the GPUI view, hidden until `show(Frame)` places it (the view's points, as the browser
    tile's page) and `hide()` takes it away. Its target is a `UIResponder` subclass whose
    paste configuration accepts what clipboard sync carries (`accepted_types`: the
    `ClipFormat` types), so the system dims it for anything else. A tap loads every accepted
    representation and gives the UI's callback one `Pasted` on the main thread: `text()`,
    `data(uti)`, and `board()`, an in-memory pasteboard whose reads never ask, for the paths
    that read one (an offer to a worker, a picture pasted into a shell). Style is the system's
    control drawn from the theme's colours and radius (`Style`); the label is the system's.
  - **What stays.** A hardware keyboard's ⌘V keeps the key path. Where the button goes (the
    key bar over remote tiles and the composer) is the UI's, not decided here.
  - Proven: builds for iOS and launches in the simulator. A tap cannot be proven by a test:
    the control refuses synthetic touches by design, which is the point of it.
- 🔬 **The worker's pasteboard alert: read the state, never the contents, to decide** (2026-09-29,
  product gaps C7 and risk 1). Since macOS 15.4 a program that reads the general pasteboard's
  contents without the person pasting raises the paste alert unless it is allowed in System
  Settings. The worker reads the clipboard on every change to keep it in step, so an alert
  would stop that silently on first use.
  - **The check.** `slopty_platform::pasteboard_access::general()` reads
    `NSPasteboard.accessBehavior`. `NSPasteboard.h` (macOS 27 SDK) describes it as the state
    and puts the alert on "programmatic pasteboard access", the contents; `Default` is "ask
    upon programmatic access" until the first alert turns it into `Ask`, and "all other
    pasteboards default to always allow access", so a named pasteboard never alerts. It maps
    to `Access::{Allowed, NotAskedYet, Asks, Denied}`; only `Allowed` `reads_freely()`, and
    `problem()` is the health line naming System Settings ▸ Privacy & Security. Linux and iOS
    answer `Allowed` (iOS asks per paste, which `Pasteboard::reads_ask` already covers).
  - **The design for the worker.** Before a read that keeps the clipboard in step, the worker
    checks `general()`. When reads are not free it reads nothing, offers nothing, and its
    doctor reports `problem()`, rather than raising an alert on a screen nobody may be
    watching. `detectPatterns`/`detectMetadata` probe without alerting, but only tell what
    kind of thing is there, not its bytes, so they cannot build an offer.
  - **Unverified: whether a LaunchAgent's first read raises the alert on macOS 27.** The live
    test `pasteboard_access::tests::reading_the_general_pasteboard_is_free_when_it_says_so`
    (ignored) answers it: the behaviour before and after one read. It must run on a Mac nobody
    is using, since the alert shows on its screen.
- ✅ **Transfers that survive pocketing the phone: iOS 26's continued processing**
  (2026-09-29, product gaps #8). An upload or download started on the phone stops when the app
  leaves the screen, because the process is suspended and the link with it.
  `BGContinuedProcessingTask` (iOS 26) runs work a person started "even if a person backgrounds
  the app", shows its progress in a Live Activity, and lets the person cancel it there.
  - **API.** `slopty_platform::continued::Work::begin(title, subtitle, expired)`, then
    `progress(done, total)` and `end(success)`; dropping it ends it as failed. Off iOS it does
    nothing. `slopty_client::xfer::upload` and `download` begin one each: an upload shows the
    worker's reported bytes, a download what each file holds (counted once across resumes).
    A cancel from the Live Activity (the task's expiry) cancels the transfer as the UI's cancel
    does: its tasks stop at the next chunk and the worker hears `Cancel`, for the attempt a
    retried download is on now.
  - **One identifier per work, registered just before it is submitted.** The scheduler refuses
    a handler for the wildcard itself (Apple DTS, developer forums thread 799126), and a second
    registration of one identifier kills the app, so each work is
    `<bundle id>.transfer.<pid>-<n>`. `Info.plist` permits `<bundle id>.transfer.*`.
  - **One queue.** The launch handler runs on a serial queue of ours, and every message to the
    scheduler and to a task goes through it, so a task is never touched from two threads. The
    progress the system hears moves only when the shown thousandth or the total does. A work
    that ends before the system starts it withdraws its request, and a start that races the
    withdrawal is completed at once with the work's outcome (`continued::ledger`).
  - **Bindings by hand for now.** objc2's `objc2-background-tasks` 0.3.2 does bind
    `BGContinuedProcessingTask`, but it is not a workspace dependency yet; the handful of
    messages are sent by selector from the SDK headers, each with its rule. Swapping to the
    crate is a workspace dependency and a mechanical change.
  - **The floor's submit.** `submitTaskRequest:error:` is deprecated in the iOS 27 SDK for a
    completion-handler twin that is iOS 27 only, above the floor, so the floor's call stays.
  - **Needs, outside this change:** `Info.plist` gets `BGTaskSchedulerPermittedIdentifiers =
    [dev.aislopware.slopty.transfer.*]` and `UIBackgroundModes = [processing]` (xtask's iOS
    project). The simulator runs no background tasks (`BGTaskSchedulerErrorCodeUnavailable`),
    so the Live Activity, the run off screen and the cancel from it need a device.
  - Tests: `continued::ledger::tests` (the state machine), `continued::tests` (what the system
    hears), `xfer::offscreen::tests` (titles, a cancel stopping the current attempt),
    `xfer::tests::a_download_counts_what_each_file_holds_once`.

- ✅ **A Linux worker holds its clipboard, and the session's `xclip`, `xsel`, `wl-copy` and
  `wl-paste` reach it** (2026-10-01, readiness audit item 13). Its board was `Unsupported`, so
  copy and paste with a Linux shell were dead both ways, and a picture pasted into Claude Code
  there could not be read.
  - **Why a held board rather than the desktop's.** A Linux worker is terminal-only and usually
    headless, a systemd user unit with no `WAYLAND_DISPLAY` or `DISPLAY`. Even on a desktop, the
    person typing in a Slopty session is at the client, so the clipboard that session's programs
    should see is the client's, not that of a screen nobody is looking at. A Wayland
    data-control or X11 board belongs with a Linux desktop stream, which does not exist yet.
  - **The board.** `slopty_input::pasteboard::Held` holds items, their types and a change
    count, and asks clipboard sync's provider for a promised type when it is first read, outside
    its lock, keeping the answer while the contents stand. Clipboard sync itself is unchanged:
    the poller announces a copy made there, a client's offer is mirrored as promises, and a
    shell's picture paste is fetched before the chord goes on. The primary selection is a
    second `Held` in the daemon, never synced, since a Mac has none.
  - **The commands.** On Linux the session's `SLOPTY_BIN` directory, first on its `PATH`, holds
    `xclip`, `xsel`, `wl-copy` and `wl-paste` as links to the `slopty` CLI
    (`shell_integration::CLIPBOARD_COMMANDS`), beside `xdg-open`. Each reads the flags programs
    pass: X toolkit prefixes for `xclip` (`-sel c`, `-t TARGETS -o`, `-rmlastnl`, `-f`), grouped
    GNU flags and the piped-ends default for `xsel`, `--type=` spellings for `wl-copy` and
    `wl-paste`, with `wl-paste`'s trailing newline and `wl-copy`'s type told from the bytes.
    Claude Code reads a picture with `xclip -selection clipboard -t TARGETS -o` and `-t
    image/png -o`, or `wl-paste -l` and `--type image/png`, with no display check, so its
    picture paste works through them. It copies through them only when a display is set, and
    by OSC 52 otherwise, which already reached the client.
  - **The wire.** `CtlRequest::Clip(ClipAsk)` on the control socket: `Types`, `Read`, `Write`
    and `Clear`, for `Selection::Clipboard` or `Primary`. Bytes travel raw after the line that
    announces them (a write's after the request, a read's after `CtlReply::ClipData`), since
    a picture in JSON would be base64. Types are named the X11 and Wayland way: the worker maps
    MIME types and the X11 names of text (`UTF8_STRING`, `STRING`, `TEXT`, `text/plain`) to its
    board's types, lists text under all of them, leaves out markers and Apple types, and turns
    a `text/uri-list` into one item a file, as a Mac copies files. A read of the Mac's general
    pasteboard is refused when it would raise the paste alert.
  - **No worker.** A shell that outlives its worker runs the system's own command of that name
    when there is one past Slopty's directory on `PATH`, and says why otherwise.
  - Tests: `pasteboard::held_tests` (a write and its count, a promise asked once and kept, a
    late answer not kept for newer contents), the worker's `clip::tests` (the listing, text's
    names, file lists), the CLI's `clipboard::tests` (Claude Code's commands and each tool's
    flags, including how a bad one fails), `golden_ctl_clip`, and live,
    `linux::the_clipboard_crosses_between_a_linux_shell_and_the_client` in `cargo xtask linux
    e2e`: an `xclip` copy reaches a watching client as an offer, and the client's text and
    promised picture are what `xclip -o`, `wl-paste` and Claude Code's picture check read there,
    the picture fetched from the client when read.

- ✅ **A download into Finder shows its progress on the file, and Finder's cancel stops it**
  (2026-10-02, readiness audit item 12). A worker's file dragged out, a folder row or the file
  grip brought down with no sign of progress and no way to stop it: the download fills a hidden
  directory beside the drop and is renamed into place at the end.
  - **Finder's own progress, not a panel of ours.** Finder draws a pie on an item, with a cancel
    button, for a progress published on its file URL (`NSProgress` of kind file, operation
    downloading), as for Safari's downloads. That is where the person dropped the file and where
    they look, so it feels like a local copy. `slopty_platform::continued::Work` is the system's
    progress UI for a transfer on both platforms: a Live Activity on iOS, and on macOS this
    progress when the work is given the file it lands as (`Work::begin`'s `at`).
  - **A placeholder holds the name.** Until the rename Finder has nothing to draw on, so an
    empty file is made at the destination when nothing is there, and removed, with the progress
    unpublished, when the work ends, before the rename. Something already there is shown on and
    left alone.
  - **The cancel.** Finder's cancel calls the progress's cancellation handler, which is the
    work's expiry: the transfer stops at its next chunk and the worker is told, as the Live
    Activity's cancel does.
  - **The path through.** `Remote::download` takes `shown_at`, the file the person sees it
    become: the destination for a drop into Finder, `None` for a paste into another worker or the
    iOS Files picker, which nobody watches land.
  - **Tested on the main thread.** Foundation hands a publish to a subscriber on the main
    thread's run loop (checked with a scratch program: never seen without it), which libtest
    never gives a test, so `crates/slopty-platform/tests/main_thread.rs` is its own harness:
    it subscribes to the destination as Finder does, sees the progress and its bytes, cancels
    through it and sees the transfer's cancel, and finds the placeholder gone. It answers a
    runner's `--list --format terse`, so nextest runs it. `continued::mac::tests` covers a file
    already there.


- ✅ **Each worker's home is a place in Finder, whose files come down as they are read**
  (2026-10-02, readiness audit item 11; the paused ruling in `docs/decisions/audio.md`, "Worker
  files paste into Finder through a File Provider domain", is built here).
  - **The extension.** `Slopty.app/Contents/PlugIns/SloptyFiles.appex` is a replicated File
    Provider extension (`com.apple.fileprovider-nonui`, principal class
    `SloptyFilesExtension`), all Rust: `apps/slopty-files`, whose `main` registers its
    `define_class!` classes and calls Foundation's `NSExtensionMain`. It is sandboxed with the
    network both ways (QUIC binds a UDP socket of its own) and the app group
    `UK58J62H8L.dev.aislopware.slopty`, which a Developer ID signs without a provisioning
    profile. No File Provider entitlement exists to ask for.
  - **What Finder sees.** One domain per worker the server lists, named for the worker, under
    Locations and at `~/Library/CloudStorage/Slopty-<worker>`. The app follows the directory it
    caches (`slopty_app::finder::follow`): a domain is added as a worker comes, added again
    under a new name as one is renamed, and removed, with the root the extension wrote for it,
    once the server forgets the worker or the app lets the server go. A change of a worker's
    load or liveness asks nothing of the system, and a domain that already matches is left
    alone (`slopty_platform::files::plan`). Its root is the worker's home,
    like a share's home folder, and every file and folder under it shows with its size, date,
    kind and the worker's hidden flag. An item's identifier is its path under the home, so the
    extension keeps no table between its launches.
  - **Lazy.** A file is dataless until something reads it. The read faults into
    `fetchContents`, which brings the bytes down with the same transfer a file tile uses into
    the domain's temporary directory and hands the system the file; the reader waits in the
    kernel meanwhile. The progress returned is cancellable, and a cancel stops the transfer.
    A folder is listed when it is opened. Finder can remove a download to free space and
    fetch it again on the next read.
  - **The one-time switch-on.** A new domain comes up switched off until the person switches
    Slopty on under System Settings ▸ General ▸ Login Items & Extensions ▸ File Providers
    (checked on this Mac, 2026-09-25). The palette's "Show workers in Finder" opens that pane
    (Finder Sync's `showExtensionManagementInterface`, the one documented way there) while any
    domain is off, and the workspace says what to do there in a notice, "Turn on Slopty under
    File Providers, then try again". Once they are on, it opens the first worker's home, by
    name, in Finder, or `~/Library/CloudStorage` while the system has not yet said where it put
    any. With no worker listed it says to connect to a server. The testing mode that skips the
    switch needs a development provisioning profile, so no product build uses it.
  - **Only a build the team signed touches the container.** The group is the team's, and
    macOS asks the person before a process reads a group container it is not entitled to, so
    `cargo run`, a test binary or an ad hoc bundle would raise that prompt. The container is
    used only when the process's own signature (`SecCodeCopySigningInformation`) names the
    team `UK58J62H8L`; any other build follows no directory, adds no domain, and says, when
    asked to show the workers, that it has no Finder extension. The self-test follows nothing
    either.
  - **Forks, each taken the way that feels most like a local disk.**
    - *Its own link.* The extension dials the workers itself, at the addresses the app writes
      to the shared container (`slopty_platform::files::Directory`), so Finder works with the
      app closed, as a mounted disk does. It checks the worker that answers is the one the
      domain is for.
    - *The whole home, not what was offered.* A domain holds the worker's whole home, open to
      browse at any time, rather than only the files of a copy, which is all the 2026-09-25
      ruling asked for.
    - *Live.* Every folder the system was given is watched on the worker (`WatchFolders`), and
      a change comes to the system through the working set (`apps/slopty-files/src/changes.rs`),
      so a file made on the worker shows in an open Finder window without a refresh. An anchor
      from an earlier run of the extension, or older than the 10 000 changes kept, is expired,
      and the system lists again.
    - *Finder writes, and nothing on the worker is written over (2026-10-10).* What Finder
      makes, renames, moves or trashes in the domain is done on the worker with the file tile's
      own ops (`FsOp`): a new folder with `MakeDir`; a new file sent up into its folder as a
      dropped file is, where a taken name lands as the next free one, which the system then
      shows; a rename or a move with `Move`, refused onto a taken name; and Move to Trash with
      `Trash`, to the worker's own trash, where the person can put it back (so a deletion made
      under the domain from a shell trashes too, and a folder still holding items is not taken
      unless the deletion is recursive). A change the worker refuses (the home, a folder into
      itself, another volume, a volume with no trash) is undone in Finder, the item put back as
      the worker has it. Two forks:
      - *Contents were read-only until the save-back below.* A file's contents were never
        written back: the worker writes nothing over, and an edit saved in place would have to.
        So a file in the domain was read-only. That waited on a replace the worker checks
        against the version the edit began from.
      - *The worker's replace (2026-10-10).* That replace is on the wire:
        `FsOp::Replace { path, with, base: FileVersion { size, modified_ms } }`. The new
        contents are sent up first, as any upload into the drop directory. The worker refuses
        unless `path` is still the version `base` names, the same size and modification time
        pair the domain's item versions are made of (`apps/slopty-files/src/item.rs`). A refusal
        is `FsRefusal::Changed { now }`, with nothing touched. Otherwise it copies the sent file
        beside `path` (a clone on APFS), keeps the file's mode, puts the copy on the device,
        and looks at `path` once more. Only then is the copy renamed over `path`, so a write
        made between the first look and the rename is refused rather than lost. The sent file
        goes after. A link at `path` keeps pointing at its file, which is the one replaced
        (`slopty_platform::fs::replace_from`, `slopty_worker::fsop`).
        Tests: `fsop::tests::a_replace_writes_only_over_the_version_seen`,
        `fs::tests::replace_from_puts_a_copy_in_place_only_when_ready`, goldens
        `client_fs_replace` and `worker_fs_refused_changed`.
      - *Finder saves in place (2026-10-10).*
        - Every item now allows writing (`AllowsWriting`, which is the same bit as adding to
          a folder) and is `UserWritable`.
        - A `modifyItem` that carries `Contents` sends the saved file up into the worker's
          drop directory (`Dest::Staging`). It then asks a `Replace` over `base`. `base` is
          read back from the `baseVersion` the system hands in: the content version the
          extension gave the item, 8 bytes of size and 8 of modification time, little
          endian (`item::version_of`).
        - A rename in the same change is done first, and the save goes to the moved item.
          The upload stops with the progress's cancel.
        - **Saved over a change on the worker.** `Changed` means the file was written on the
          worker meanwhile, or the base is not one the extension made. The worker's file is
          then kept as it is, and the save is moved beside it as "notes (conflicted copy).txt",
          numbered from 2 when that name is taken. If it cannot be moved there (another
          volume), it is sent up anew under that name.
        - **What the system is told.** It is answered with the worker's item and told to fetch
          it again, so Finder shows the worker's file and the copy appears beside it. Nothing
          of either side is lost, as Dropbox and iCloud keep a conflicted copy.
        - **A refused or failed save.** It is undone the same way, the worker's contents
          fetched back.
        - Tests: `tests/domain.rs`
          `a_file_saved_in_finder_replaces_only_the_version_it_was_opened_at` (a real worker:
          replaced at the version seen, kept with a copy over a change, numbered, and a base
          not known), `item::tests::a_content_version_names_the_files_version`, and
          `domain::tests::a_conflicted_copy_is_named_beside_its_file`.
      - *A rename changes the item's identifier.* The identifier is the path, so a moved item
        comes back under its new one, which the system takes as the moved item merged into
        the one at the new place (the header's merge rule); a child of a folder moved before it
        is then the item already where it went. Links, aliases and packages stay on the Mac,
        out of sync (`NSFileProviderErrorExcludedFromSync`), and an item made again after the
        domain was reset is the one already on the worker, so nothing is sent twice.
  - **Where the root is.** The app needs the root's path to name worker files by, but a process
    that asks the system for it (`getUserVisibleURLForItemIdentifier`) may never read the
    domain's files after (`EDEADLK`). The extension never reads its own files, so it asks, and
    writes the answer to the shared container (`slopty_platform::files::root`).
  - **Signing.** `cargo xtask bundle` builds the extension, writes its `Info.plist`, and signs
    it under `dev.aislopware.slopty.files` with its entitlements before the app, which takes
    the group too.
  - **A fetch outlives its link.** While a fetch runs, the domain dials its worker again each
    time the link goes, and the transfer goes on over the new link from what it holds
    (`docs/decisions/transport.md`, "A download outlives its link"). Pasting copied worker
    files into Finder, the reason for the 2026-09-25 ruling, uses the roots above
    (`docs/decisions/audio.md`, "Worker files paste into Finder").
  - **A big folder pages (2026-10-10).** The system enumerates a folder page by page, and each
    page is one of the worker's (`FOLDER_ENTRIES`, 2000): the system's first page asks for the
    folder's first (`ListFolder`), and the extension hands back, as the next page's bytes, the
    worker's cursor (`FolderPage`'s `After`, with the count told so far) for the page after it
    (`apps/slopty-files/src/pages.rs`). So the extension keeps nothing between pages or across
    its launches, and an entry added or removed between pages neither repeats nor is skipped.
    A page's bytes are at most 500; a name too long for them (only HFS+ holds one) is cut at a
    character, so the next page starts a little early and repeats a few entries rather than
    skipping one. A full page asks for one more, which ends empty when nothing came meanwhile.
    The listing the working set's changes are told against is the whole folder: the system's
    pages are joined once its last comes, and a watched folder's relist, which the worker sends
    as its first page, is made whole with its pages before it is compared, so the entries past
    the first page are never taken for deletions. An item is looked up in its folder's whole
    listing. The relists wait in an unbounded queue, each folder's latest kept, so the link's
    reader never waits on the domain while the domain waits on a page the reader brings.
  - **Not yet.** The fetch's progress moves only at its end.
  - Tests: `files::tests` (slopty-platform: the directory, the roots, one domain per worker and
    none for a forgotten one, and no container for a build the team did not sign),
    `finder::tests` (slopty-app: the switch before a worker's home, and each notice),
    `the_palette_offers_the_workers_in_finder_on_a_mac`, `bundle::tests` (xtask: the
    extension's place in `PlugIns`, its plist and entitlements), `item::tests` and
    `changes::tests` and `pages::tests` (slopty-files: a 4500-entry paged fake told in three
    pages each entry once, entries added and removed between pages, a page that is exactly
    full, a long name cut without a skip, the system's own first pages, and a folder gathered
    whole), and `tests/domain.rs` against a real worker daemon: a folder of 4500 files paged
    through from each page's bytes, an item past the first page, and a change past it logged
    alone (`a_big_folder_pages_through_and_its_changes_stay_whole`), the home lists, a deeper item is found, a file's bytes come down with its version, a file made,
    changed or removed on the worker reaches the working set, and a missing item, a file asked to
    list, a forgotten worker, another worker at the address and an address nobody answers each
    fail as the system is told, a fetch goes on once its worker restarts on its port
    (`a_fetch_goes_on_once_the_worker_is_back`), and a copied file's URL names the domain's
    file (`a_copied_files_url_in_the_place_is_the_domains_file`). The switch-on and Finder's
    own reads are not proved yet: they need a person or a provisioning profile, so they go to a
    macOS guest (`cargo xtask vm`).

- ✅ **A Linux worker's files, search, uploads and tunnels are proven in the container, and its
  crates are tested on a Linux runner** (2026-10-03, readiness audit C5). The Linux worker had
  run only in the local Docker e2e (a shell, folders, reads, an agent's status, the clipboard),
  and no gate lane ran on Linux but the tools'.
  - **Two more live cases** (`crates/slopty-e2e/tests/linux.rs`, `cargo xtask linux e2e`).
    Files change through `docker exec` as the worker's account, so inotify hears another
    process write them, as it would an editor.
    - `a_linux_worker_follows_files_on_inotify_and_searches_them`: a file tile's file, written
      there, is read again unasked; a folder tile's folder is listed again with a new entry; the
      file's removal says it is missing; a text search over a tree streams its one match, with
      the build `.gitignore` names left out.
    - `an_upload_lands_in_a_linux_shell_and_its_server_is_tunnelled_here`: the app's own link
      (`WorkerLink::start_forwarding`) uploads 2.5 MB into the shell's directory, read from
      `/proc/<pid>/cwd`, whole by its digest there; a server the shell starts (perl, which Debian
      always has) is found by the Linux port scan, forwarded to this Mac's loopback, and echoes
      through the tunnel.
    - Numbers: `docs/MEASUREMENTS.md`, 2026-10-03.
  - **`linux e2e` no longer builds the workspace here first.** Its tests spawn nothing on this
    Mac, so it sets `SLOPTY_BINS_FRESH` and nextest's setup script returns at once, where it
    used to build every binary of the workspace with its tests (and fail on any crate another
    session had half-edited).
  - **CI's Linux lane** is `docs/decisions/tooling.md`, "The Linux worker is built and tested on
    a Linux runner".

- ✅ **A server that turns the app away says where the grant goes** (2026-10-03, readiness audit
  D5). A server on a tagged node, or on another user's, admits a device only with a tailnet grant
  carrying the client role (`docs/decisions/topology.md`, "Admission asks whois; roles come from
  grants"). The status bar said only that the policy did not grant it.
  - The status bar now reads "Server needs a tailnet grant for this device", and the first
    refusal, not each redial's, shows the notice "Copy the tailnet grant from the palette into
    Tailscale's Access controls": the admin console's policy file, or Headscale's. Both fit a
    notice's single line. The server panel's Connect, refused the same way, says both.
  - The palette's "Copy the tailnet grant for this server's clients" puts the grant on the
    clipboard (`slopty_app::server::client_grant`), as the policy file's `grants` takes it:
    `{"src": ["autogroup:member"], "dst": ["tag:slopty-server"], "ip": ["*"], "app":
    {"github.com/aislopware/slopty": [{"roles": ["client"]}]}}`, the tag discovery prefers. It
    is always in the palette, since the grant is the tailnet's, not this device's: whoever runs
    the policy copies it from any client. It was first only in the log, where nobody reads.
  - Tests: `server::tests::a_refused_device_is_told_where_to_grant_it_once` and
    `the_grant_is_copied_for_the_policy_file`.

- ✅ **The Linux lane's first run: what was Linux's and what was the Mac's in the tests**
  (2026-10-03). CI's first Linux run failed 15 tests. Each was read as a Linux bug until shown
  to be a Mac assumption, and each now asserts what is right on the platform it runs on.
  - **Opening a pseudo-terminal.** The open of `/dev/ptmx` was made again on XNU's
    `EREDRIVEOPEN` (-6) and on its ENXIO while the table of pairs grows. Neither exists on
    Linux, and rustix cannot even hold a negative errno there (its debug assertion was the
    failure). Both retries, and their tests, are macOS's alone now. Linux's devpts refuses an
    open only at its limit, with ENOSPC, which is now said as every pseudo-terminal being in use
    with `kernel.pty.max` named, as ENXIO names `kern.tty.ptmx_max` on macOS
    (`running_out_of_pseudo_terminals_says_so`, on both).
  - **Mac wording in tests.** GNU `dd` ends its statistics with "copied", BSD's with
    "transferred" (`the_tty_is_the_controlling_terminal_of_the_child`). `/bin/sh` is a program
    that runs bash on macOS and a link named `sh` on Linux, and the foreground process is named
    as it was started (`a_ptys_foreground_process_is_the_program_it_runs`).
  - **UDP on Linux segments every transmit.** noq-udp turns on UDP GSO on any Linux since
    4.18, so a transmit there carries up to 64 datagrams on the plain path too. The test sends
    as noq does, transmits of at most that many, and so covers GSO on Linux
    (`datagrams_arrive_whole_on_the_plain_path`).
  - **A peer that is this machine but not loopback.** The admission tests reached the worker at
    `fe80::1%1`, the link-local address macOS gives `lo0`. Linux's `lo` has none, so on Linux the
    peer is the address this machine sends from on its default route (the packets never leave
    it). The admitted range became 192.0.2.0/24, which no host is given (RFC 5737), since a
    GitHub runner's own address is in 10/8. The admission itself was right.
  - **The receive buffer** is `docs/decisions/transport.md`, "A Linux socket's receive buffer".
  - **Shells.** The zsh tests need zsh, which Ubuntu does not ship, so CI's Linux job installs
    zsh and fish. fish is found at `/usr/bin/fish` too, so the fish cases run on Linux.
  - **zsh on Debian and Ubuntu lost its cursor and its fallback marks.** Their
    `/etc/zsh/zshrc`, read after the integration is sourced from `.zshenv`, sets
    `zle-line-init` and `zle-line-finish` by hand (for the keypad's application mode), which
    replaced Slopty's: no bar cursor at the prompt, and no `133;P` for a prompt a theme had
    rebuilt. The zle hooks now go in at the first prompt, from the first precmd, once every
    startup file has run, and wrap whatever widget is there then, as ghostty's deferred setup
    does (`an_interactive_zsh_emits_prompt_marks`,
    `a_theme_that_rebuilds_ps1_still_gets_its_prompt_marked`). The handoff test typed `open`,
    which Linux's opener is not: it types `OPENER` (`xdg-open` there).
  - **A process's descriptors and threads on Linux** come from `/proc/<pid>/fd` and
    `/proc/<pid>/task` in `slopty_testkit::process`, which returned nothing off Apple
    platforms, so `a_closed_session_gives_back_its_descriptor` had nothing to compare.
  - **A test that hung one run in fifteen.**
    `a_worker_that_dies_mid_attach_leaves_the_session_to_the_next` read ptyd's 12 MB reply
    with tokio's `read`. Linux ends a read at the bytes that carry a descriptor, and tokio
    takes a short read for a drained socket and waits for an edge that never comes, since
    ptyd is itself waiting for room. The worker reads with `recvmsg`
    (`fdpass::Inbox`), which tokio wakes only on `EAGAIN`, so the product was right and the test
    now reads the same way: 150 runs, no hang, where 2 in 30 hung before.
  - **A received descriptor closes on exec from the start on Linux.** `Inbox::recv` asks for
    `MSG_CMSG_CLOEXEC` there; macOS has no such flag and keeps setting it right after
    (`a_received_descriptor_closes_on_exec`).

- ✅ **Slopty sets this Mac up only from Applications, and follows the bundle when it moves**
  (2026-10-04, readiness audit N26). The daemons run in place from the bundle on the home
  volume. Opened from Downloads, the bundle runs from the read-only copy App Translocation
  makes, which a restart removes. The LaunchAgents and the Claude Code hooks then point at a
  path that is gone. A disk image is gone once it ejects.
  - **Refused, with the way on.** "Use this Mac" and the install or server setup on this Mac
    stop before changing anything when the bundle is outside `/Applications` and
    `~/Applications` (`this_mac::misplaced`). The checklist offers "Move to Applications": `ditto`
    copies the bundle to `/Applications`, or to `~/Applications` when this user may not write
    there, after moving a copy already there to the Trash. The quarantine flag comes off the
    copy, since this copy already passed Gatekeeper to run and a flagged copy would be
    translocated again. The app then opens the copy and quits. A build run from a source tree
    is no bundle and is never refused.
  - **Re-pointed at launch.** When the worker's LaunchAgent runs a binary in another bundle and
    that binary is gone, the app installs again from itself (`this_mac::repoints`). Its ptyd
    keeps its sessions when its custody is the same. An install that would end sessions is said
    and not done.
  - Tests: `slopty-app` `this_mac::tests::slopty_runs_its_worker_only_from_applications`,
    `this_mac_asks_before_it_ends_sessions_or_installs_from_a_download`.

- ✅ **A phone or iPad finds the server from a Mac's code** (2026-10-05). The palette's "Connect a
  phone or iPad" shows a QR code of `slopty://connect?server=<host:port>`, with the address in
  type under it and "Copy link" for a device with no camera to hand.
  - **The device's own Camera reads it.** iOS hands the link to Slopty through its URL scheme
    (`CFBundleURLTypes`, scheme `slopty`), so Slopty asks for no camera and draws no scanner. The
    scene's delegate gives each link to `slopty_app::open_link`, at a launch from its connection
    options and later from `scene:openURLContexts:`. Links wait in an inbox until the workspace
    listens. `gpui_ios` lets no embedder register GPUI's own open-URL callback, so the links do
    not go through it.
  - **A link fills in and never connects.** Any page or app can open such a link, so it opens
    "Connect to a server" with the address in the field, and the person presses Connect. Only
    the exact form is taken: that scheme and host, one `server` pair, a DNS name or IP address,
    and an explicit port. Anything else is dropped, logged at debug.
  - **The address the device reaches.** A server elsewhere is given as this app reaches it. A
    server on this Mac, which the app reaches over loopback, is given by this Mac's `MagicDNS`
    name, or by its tailnet address where `MagicDNS` is off. With no tailnet, the dialog says
    to type the VPN address on the device.
  - **Dark on light in both appearances.** The modules are the light theme's text on its page,
    with the standard four-module quiet zone, at medium error correction, in whole points per
    module. Scanners need dark on light, and many do not read an inverted code.
  - Tests: `slopty-app` `invite::tests` (strict parsing, round trip, quiet zone, contrast, the
    field filled without a connect, Esc). `slopty-e2e` `gallery::the_code_for_a_phone_names_this_mac_on_the_tailnet`
    (goldens `invite`, `invite-dark`, the code and its address masked since the port is the
    run's) and `ios::a_link_from_a_mac_s_code_fills_the_server_s_address`. The simulator test
    hands the link in where the scene's delegate does (`Command::OpenLink`), because `simctl
    openurl` stops at the system's "Open in Slopty?" confirmation, which only a person may
    answer.
  - **A launch with no link.** `-[UISceneConnectionOptions URLContexts]` is marked nonnull but
    returns nil on a launch without a link, and objc2's generated getter panics on that. The
    delegate reads it as optional through `msg_send!`.

- ✅ **Notifications turned off are said, not dropped unseen** (2026-10-05, readiness G1). With
  notifications denied, every note was dropped at debug level, "needs you" included, and nothing
  on screen said so. Slopty's place in Finder had the same blind spot: it shows only once the
  person switches its File Provider on.
  - **This Mac's checklist carries the app's own two lines** under the worker's (`this_mac::app_lines`):
    Notifications and Finder. Notifications never asked for offer "Allow", which raises the
    system's prompt from the person's own press. Turned off, they open the app's page of
    Notifications settings. Finder switched off opens the File Provider list. A dev build with
    no app bundle or no extension says so with nothing to press. Neither line is in the way of
    adding this Mac, so the flow does not wait on them. But once the server lists this Mac, a
    line with a button left keeps the panel open, ready, with "Done", and closes it by itself
    when the person comes back with both on. Both lines are read again whenever the app
    comes back to the front.
  - **Once, on coming back.** `Notifier::alerts` says how notes stand. When a note went out
    while notifications were off, the app says so the first time it comes back to the front
    in a run, as a notice ("Notifications are off. Turn them on in System Settings."). It is
    said then because that is when the person can see it, and a note dropped while away is
    what it is about.
  - **Turned on again, they go out.** `notify::System` read the settings once and stayed
    denied for the rest of the run. Now a note posted while denied reads the settings again,
    so notes turned on in System Settings go out from the next one.
  - Tests: `slopty-app` `this_mac::tests::the_apps_own_lines_say_what_it_lacks`,
    `this_mac_stays_open_on_what_the_app_lacks_until_it_is_turned_on`; `slopty-ui`
    `notes_turned_off_are_said_once_on_coming_back`; the `this-mac` golden's two new lines.

- ✅ **JMango's Developer ID signs Slopty** (the person, 2026-10-05). The team is `UK58J62H8L`
  (JMANGO VIETNAM OPERATIONS COMPANY LIMITED), in place of `AJ4R8GWM7A`, with its certificate
  imported into this Mac's keychain. `slopty_platform::files::TEAM` and the app group
  `UK58J62H8L.dev.aislopware.slopty` follow it, and xtask's `sign::TEAM` with them, which a
  test holds equal.
  - The keychain holds two Developer ID Application certificates. xtask took the first, which
    was the other team's, so it now takes only the team's (`sign::pick_identity`). Another
    team's signature has no right to the app group: the build would lose its shared container
    without a word.
  - A grant made to the old team's builds doesn't carry over, since a designated requirement
    names the team. Screen Recording and Accessibility are asked again once for the re-signed
    worker. Notarising still needs the App Store Connect key, and CI's release job the
    certificate, both as secrets.
  - Another team builds signed with its own Developer ID by setting `SLOPTY_TEAM`
    (2026-10-05, the person's ask). The team is a build input: `.cargo/config.toml` sets
    JMango's, an environment value stands over it, and `files::TEAM`, `files::GROUP`, xtask's
    `sign::TEAM` and the bundle's group are all read from it at compile time (`env!`).
    `sign`'s tests are written against whatever team was built in, and they pass under another.

- ✅ **Slopty opens at login, and the system holds that** (readiness N4, 2026-10-06). After a
  restart the Mac app wasn't running until someone opened it. Its server then had no seat to
  tell the person that an agent needed them. Slopty is now its own login item
  (`SMAppService.mainApp`, `slopty_platform::login`).
  - **Set when "Use this Mac" finishes.** A Mac that shares itself should be up to say so.
    It is set only when the item is off. One the person turned off in System Settings, under
    Login Items (`requiresApproval`), stays off, because registering it again would not allow
    it anyway.
  - **A checklist line and a settings switch, both reading the system.** This Mac's checklist
    has an "Open at login" line among the app's own lines, with "Turn on" when the item is off
    and "Open settings" at Login Items when it was turned off there. The settings form has a
    switch in "This app". No key in `settings.toml` holds it. Login Items changes it as well,
    so a copy in the file would drift from it, and the app would end up fighting the person's
    choice made there. The form reads the system as it opens and again each time the window
    comes back to the front, and it sets the item off the main thread, turning the switch at
    once. A build outside an app bundle has no login item (`Login::Unavailable`): the line
    says so and the switch isn't shown.
  - **The add panel's body scrolls.** With the checklist's eighth line, the panel ran past a
    700 pt window and its way back went out of view. The panel now gives up height to the
    window, and only its body scrolls.
  - Registering posts the system's "Login item added" note, so no test registers: the app's
    and the self-test's hosts stand in for it, and the form takes its way to the item as a
    global the app installs. Tests: `slopty-platform` `tests/login.rs` (status only),
    `slopty-app` `this_mac::tests::the_apps_own_lines_say_what_it_lacks`,
    `this_mac_runs_the_server_then_waits_for_it_to_list_the_worker`,
    `this_mac_checks_what_the_app_lacks_and_reads_it_again`,
    `a_failed_install_offers_another_try`; `slopty-ui`
    `settings_form::tests::open_at_login_is_the_systems_switch`; the `this-mac` golden's new
    line.

- ✅ **Needs you is Time Sensitive; nothing else is** (readiness N5, 2026-10-06). Under a Work
  focus or Notification Summary, the one note that matters waited in the pile with the rest.
  A note now carries `urgent` (`slopty_platform::notify::Note`), which sets
  `UNNotificationInterruptionLevelTimeSensitive`. The workspace's attention sets it only for
  an agent that needs the person: the look's asks, with their approval buttons or without, and
  the server's `NeedsYou` notices, a project's included. A finished turn, a failure, a long
  command and a program's own note stay at the active level. Two reasons: the system shows
  the person how often an app breaks through, and lets them take that away, and the 10-04
  APNs ruling already picked the same split.
  - **No entitlement yet.** `com.apple.developer.usernotifications.time-sensitive` is a
    portal capability, so it needs a provisioning profile. The Developer ID app embeds none,
    and AMFI refuses to launch a binary that claims an unprovisioned `com.apple.developer`
    entitlement, so adding it now would break every signed build. Without it, the system
    shows the note at the active level, and nothing fails. Enabling the capability and its
    profiles for the Mac and iOS is the person's step (readiness 10-06 §5). After that it is
    one line in each entitlements writer (`xtask/src/bundle.rs`, `xtask/src/ios.rs`).
  - Test: `slopty-ui` `workspace::attention::tests::only_needs_you_breaks_through_a_focus`.

- ✅ **Notes reach a pocketed phone: the server pushes, sealed, straight to APNs with the
  person's key** (readiness N6, 2026-10-06; `.research/push-2026-10-06.md`). It replaces the
  deferral from 2026-09-28. A phone's links end a few tens of seconds after it leaves the
  screen, so local notes stop there. Now the server pushes what a linked client would have
  heard, and APNs carries it as ciphertext.
  - **Who gets a push.** The phone sends `ToServer::PushDevice { client, device }` on every link:
    its APNs token, the public half of an X25519 key only it holds, its topic and its
    slow-command time. `device: None` withdraws it, on any link. The server keeps phones by
    client id in `push.json`. A notice that finds the person at none of their clients
    (`route` falls through to every link) is pushed to each phone not listening on a live link:
    its link is gone, or it said `Presence { listening: false }` before the system suspended
    it. A finished turn shorter than that phone's slow-command time is not pushed, as a linked
    phone would not post it. Notices come only on a change of rung, so a ladder ranked again
    pushes nothing.
  - **What is pushed.** The server's own `Notice`, sealed: `PushBody { notice, ask }`, where
    `ask` is the thread's request when it is a plain yes or no a note's buttons answer
    (`RequestCard::answerable`, which the app's notes use too). It is postcard, cut to fit APNs'
    4 KB, and sealed with HPKE (X25519, HKDF-SHA256, ChaCha20-Poly1305, base mode) to the
    phone's key, with the token as associated data so it opens on no other phone. APNs sees fixed
    words ("Slopty", "An agent needs you" when it is urgent, else "An agent has news"),
    `mutable-content`, Time Sensitive for *Needs you* only, and thread and collapse ids that
    are hashes keyed by the phone's key. The phone's notification extension opens the body and
    shows what the app would have posted (P5).
  - **No relay.** A Cloudflare Worker that held the team's APNs key and forwarded signed
    requests was deleted on 2026-10-10 ("The push relay is deleted", below). The server sends
    straight to APNs with the person's own key.
  - **Off until set up.** `[server.push] apns_key`, `key_id` and `team_id` send straight to APNs
    with the person's own `.p8`. The `.p8`, the App IDs and the capabilities stay with the
    person.
  - **The provider token.** ES256, with `iat` fixed to a half-hour bucket and RFC 6979
    deterministic signatures. A server started again then makes the same token, byte for byte,
    and it changes once in 30 minutes, inside Apple's 20 to 60.
  - **The server's HTTPS client** is hyper 1 over HTTP/2 with rustls on ring and the platform
    verifier. ring needs no CMake, so the static musl server builds with zig as its only C
    compiler (`docs/decisions/tooling.md`, "Linux clippy compiles the build scripts' C with
    zig"). `NSURLSession` would have left the Linux server with no push.
  - **What a send does.** APNs' 410 or `BadDeviceToken` forgets the phone. A 429, a 5xx or a
    provider token being renewed is tried again after 2 s and 10 s, then dropped.
  - **The phone's half.** The app makes the X25519 key on first use. It keeps the key and the
    device token in the Keychain group it shares with its notification extension, readable
    after the first unlock and never backed up (`slopty_platform::notify::pushed`). It sends
    `PushDevice` on every link, on each new token and on each move to or from the front, after
    reading again whether notes reach the person. While they don't (turned off, or not asked
    yet), it sends `None`, since a pushed note would not show either. Five seconds before the
    background grace runs out, or at once when the system grants none, it says
    `listening: false`. From then on the server pushes and the app posts none of the server's
    notices (`Attention::set_listening`), so a moment is said once. Back in front, it listens
    again.
  - **The extension** (`apps/slopty-notify`, a Rust appex) opens the body with the key and the
    token and shows `note_of(body)`: the id, words, `userInfo`, category and urgency the app's
    own note would have, so a tap, Allow and Deny route as they do on a local note. A body that
    does not open shows APNs' fixed words. Its work is one function in `slopty-platform`
    (`pushed::note_from`), which the self-test runs too.
  - **What a simulator can prove.** `simctl push` does not go through APNs: the simulator's
    bridge hands the payload straight to the notification centre (its log says
    `CoreSimulatorBridge … Adding notification request`), so the extension never starts and
    the note shows APNs' fixed words. The simulator test therefore proves the rest: the
    payload is one the system takes, its fallback reads "Slopty" and "An agent needs you", and
    the body opens with the key and the token the app kept in the Keychain it shares with the
    extension, under the simulator build's real entitlements (2026-10-06). Only a real APNs
    sandbox push, with the person's own APNs key, starts the extension itself; a simulator on
    Apple silicon receives those, so that check needs the key and nothing else.
  - **This Mac's checklist** has a "Notes on your phone" line while the server runs on this
    Mac, whose settings decide it. While `[server.push]` is off the line is quiet and not in the
    way, and Set up opens Settings with the keyboard in the APNs key's field. A server on another
    machine is set up there, so no line shows for one. The line holds no panel open. The
    flow's Done moved to the panel's foot, where Cancel stood, so a checklist one line longer
    still ends with its last word in view at 700 pt.
  - Taking a pushed note back once it is answered elsewhere: see "A note answered elsewhere
    leaves a pocketed phone" (2026-10-10).
  - Tests:
    - `slopty-push`: the seal opens only with the phone's key and token, and the APNs request;
    - `slopty-server`: `hub::ladder::tests::needs_you_pushes_once_per_ask`,
      `store::tests::the_phones_outlive_the_server` and
      `push::tests::a_long_notice_still_fits_apns`;
    - `tests/push.rs` `a_notice_reaches_the_phone_and_is_taken_back`: a server, a phone that
      stops listening and a worker's thread needing the person, straight to a stand-in APNs
      over HTTP/2 and TLS on loopback with a test `.p8`;
    - `slopty-ui` `a_pushed_note_is_the_note_the_app_would_post`: the opened note is the
      app's own, and a phone that stopped listening posts none of the server's notices;
    - `slopty-client` `the_phone_goes_on_each_change_and_to_each_new_link`;
    - `slopty-app` `push::tests` (what is sent, and when listening stops) and
      `this_mac::tests::a_server_here_says_how_notes_reach_a_phone`; `slopty-ui`
      `settings_form::tests::a_named_setting_opens_on_its_row`;
    - `slopty-e2e` `ios_uikit` `a_sealed_push_shows_its_fallback_and_opens_with_the_kept_key`
      on a simulator: a token handed over as the app delegate would, notes allowed
      provisionally (no prompt), a notice sealed to the phone's key, `simctl push`, then the
      shown note and the opened one read back over the test socket.

- ✅ **The Mac window is never narrower than a phone** (2026-10-06,
  `.research/responsive-2026-10-06.md`, finding 13). No minimum was set, so AppKit let the
  window shrink below the 375 pt phone the layout is drawn for, and at 300 pt the phone bar's
  lights, title, bell and "…" ran into each other. The main window now has
  `window_min_size` 375 × 480 pt (`window::MIN_SIZE`): every size it can take is one the
  layout was drawn for. A frame kept from a smaller window opens at the floor where it stood.
  Popped-out streams keep their own sizes. Test: `window::tests::the_window_is_never_narrower_than_a_phone`.

- ✅ **Back in front, notes that went stale while away are taken down** (readiness R4,
  2026-10-09). Before, the app's own notes stayed in Notification Centre after the person came
  back, and so did the notes a push put up while the phone was pocketed, which the app never
  posted and so never withdrew. Now the app checks them each time it comes to the front:
  - `Attention::set_active(true)` withdraws the notes it posted, except an ask still waiting on
    the person. That note's Allow and Deny still work, and it goes once the ask is answered
    anywhere.
  - The app then asks the system for every note it shows (`notify::delivered`) and hands their
    ids to `Attention::delivered`. Pushed notes use the same ids as posted ones
    (`notify::pushed::note_id`), carried in the note's `info::NOTE` (corrected 2026-10-10:
    iOS shows a pushed note under its `apns-collapse-id`, so the identifier alone never
    matched). A pushed ask that is still live is adopted as the app's own,
    so its answer takes it down. Every other note is withdrawn. The test notifier keeps the
    list the same way the system does, so the behaviour is tested on the host.
  - Taking a note down while the phone stays pocketed came on 2026-10-10 ("A note answered
    elsewhere leaves a pocketed phone").
  - Tests: `slopty-platform` `notify::tests::memory_keeps_what_the_notification_centre_shows`;
    `slopty-ui` `workspace::tests::attention::back_in_front_stale_pushed_notes_go_and_a_live_ask_stays`.

- ✅ **A note answered elsewhere leaves a pocketed phone** (2026-10-10, readiness R4's M
  part). A pushed ask stayed on the phone's lock screen after the person answered it at the
  Mac or in the terminal, until the app was next opened. Its Allow then answered nothing.
  - **What iOS names a pushed note.** A remote note's identifier is the push's
    `apns-collapse-id`. Apple's `UNNotificationRequest.identifier` page says: "For remote
    notifications, the system sets this property to the value of the `apns-collapse-id` key".
    The server's collapse id is opaque, a hash keyed by the phone's key, so the identifier
    tells the app nothing. The extension now writes the note's own id into its `userInfo`
    (`info::NOTE`, set by `notify::content_of` for every note). `notify::delivered` and a tap
    report that id. The notifier remembers the identifier each note is shown under, so
    `Notifier::withdraw` by the app's id takes the pushed note down too.
  - **The server takes it back.** `Phones` keeps, per phone, the threads it was last pushed
    an ask about. A later note for the same thread replaces it under the same collapse id, and
    then the entry goes. On every ranking, before the ladder is compared, an ask whose thread
    no longer needs the person, or has ended, is taken back (`Phones::take_back`). A thread on
    a worker that is not linked is left alone, since it may still be asking. A phone that
    listens on its link again forgets its entries, since the app sweeps its own notes when it
    comes to the front.
  - **The push.** `Sending::TakeBack` names the subjects. `push::sealed` turns each into the
    collapse id its note was pushed under. `apns::What::TakeBack` is a background push:
    `apns-push-type: background`, priority 5 (APNs takes no other for it), no collapse id,
    and the payload `{"aps":{"content-available":1},"w":[ids]}` with at most
    `MAX_TAKE_BACK` (16) ids. Nothing of the note is in it, and the ids are the ones Apple
    already saw.
  - **The phone.** `UIBackgroundModes` gains `remote-notification`. The app delegate's
    `application:didReceiveRemoteNotification:fetchCompletionHandler:` reads the ids
    (`pushed::take_back_of`) and calls `notify::take_back`. That removes them, then asks for a
    listing and calls the handler on its answer, so the app is not suspended before the
    removal is out. UIKit wakes or launches the app with no scene for it, so GPUI does not
    start.
  - **Limits.** iOS budgets background pushes and drops them for an app the person
    force-quit, so the sweep when the app comes back to the front is the one that always
    holds. An ask answered on the phone's own note gets a take-back for a note already gone,
    which spends one push of that budget.
  - Tests: `slopty-push` `a_take_back_wakes_the_app_and_shows_nothing` and
    `a_take_back_goes_through_as_a_background_push`; `slopty-server`
    `hub::ladder::tests::a_pushed_ask_answered_elsewhere_is_taken_back` (answered and done at
    once, ended, swept by a phone back in front, kept for a worker away), the take-back step
    of `needs_you_pushes_once_per_ask`, and `tests/push.rs`
    `a_notice_reaches_the_phone_and_is_taken_back` (straight to the stand-in APNs, the
    take-back naming the `apns-collapse-id` the
    note was shown under); `slopty-platform` `a_take_back_names_only_opaque_ids`. The app
    delegate's half is proved only on a device, or by a simulator test sending `simctl push`,
    which is still to be written.

- ✅ **A program waiting on the person reaches a pocketed phone** (2026-10-10). A program that
  says it is blocked through its own status record (`OSC 7501`) made a note on each linked
  client, which posts a program's records itself. A phone whose link had gone heard nothing,
  because the server ranks agents' threads only.
  - **The server reads the records it already has.** Each terminal's records ride its summary
    (`SessionSummary::program`), and the worker sends the summary again whenever they change.
    When a terminal's records come to wait on the person (`ladder::program_moved`), the server
    makes a notice about `Subject::Terminal(term)`. It does this only where no agent's thread
    is seated at that terminal, since the thread speaks for it there. The title is the
    record's title, else its program, else the terminal's. The text is the record's message,
    else a line for what it needs (approval, an answer, a sign-in).
  - **Only pushed.** It goes through the same routing as a thread's notice, but only when the
    person is at none of their clients, and then only to phones. No link is told, because
    every linked client already posts the program's own note. A client that hears one anyway
    makes nothing of it (`WorkspaceView::heard`).
  - **Taken back** like an answered ask (`Phones::program_answered`, see "A note answered
    elsewhere leaves a pocketed phone"): when the records stop waiting, or the terminal
    closes. A worker that links again takes back what no longer waits
    (`ladder::programs_back`). A wait that began while it was away is no news, as a thread
    first seen is not.
  - **On the phone** the note leads to the terminal (`info::SESSION`) under the session's id,
    the same id the app's own note uses, so the two replace each other.
  - Tests: `slopty-server`
    `hub::ladder::tests::a_program_waiting_on_the_person_is_pushed_and_taken_back`;
    `slopty-platform` `a_pushed_note_carries_what_a_tap_routes_by` (its terminal case);
    golden `push_body_program`.

- ✅ **A take-back is never lost, and a late note never follows it** (2026-10-10, readiness
  10-10 rank 11). Four ways a pocketed phone was left showing an answered ask, or never heard
  of one:
  - **A full push queue.** A thread was struck from what its phone was pushed before the
    take-back was queued. If the queue was full, the take-back was dropped and never tried
    again. Now it stays owed (`Phones::owed`) until the queue takes it. Each ranking pass
    tries again, and while any is owed a try is set for `TAKE_BACK_RETRY` (2 s) later
    (`Board::retry_owed`). A newer note about the same thread or terminal replaces the old
    note on the phone, so it drops the take-back owed for it.
  - **A restart.** What each phone shows and is owed lived only in memory. It is now kept
    with the phones in `push.json` (`PushKept`). A server that starts again from the store
    takes back what was answered while it was away. The file's shape changed: an old one is
    set aside, and a phone gives its device again on its next link.
  - **A late note.** APNs' try-later sent a note again at 2 s and 10 s, alongside the pushes
    queued after it, so a note could land after its own take-back and show the answered ask
    again. Each push is now numbered per phone and subject (`push::Latest`). A retry that a
    later push about the same subject overtook is given up, and a take-back retried keeps only
    the subjects still its own.
  - **A notice no link took.** A notice for the desk or handheld the person is at went by
    `try_send` on that link alone. When every link it went to was full, nothing stood in.
    Now it is pushed as though the person were away, and the phone whose own link refused
    it counts as not listening.
  - Tests: `slopty-server` `hub::ladder::tests::a_take_back_a_full_queue_refused_is_owed_and_kept`
    (owed, tried again on the timer, and kept across a restart from the store),
    `hub::ladder::tests::a_notice_no_link_could_take_is_pushed`, and
    `push::tests::a_note_older_than_its_take_back_is_not_sent_again`.

- ✅ **A need told at the desk reaches the phone once the person leaves it** (2026-10-12,
  readiness 10-12 rank 1). A need that came while the person sat at their desk was routed
  there alone and never pushed. If they then walked away, their phone stayed silent until
  the next need came, however long this one waited.
  - Now, when a desk goes from active to not, or its link drops while active, the server
    reroutes each thread still at Needs you (`ladder::left`). If the person is now at no
    client, it pushes each one to every phone not already showing it, as though they had
    been away when it came.
  - The kept push state (`Phones::asked`, stored in `push.json`) is what keeps this to once
    per wait. A phone already showing the thread is skipped (`Phones::push_new`), so leaving
    the desk again, or its link going, pushes nothing more. The links are not told again,
    because every client already holds the notice.
  - Leaving a phone pushes nothing. A notice the person saw on the phone in their hand goes
    with them; only a desk stays behind.
  - Test: `slopty-server`
    `hub::ladder::tests::a_need_told_at_the_desk_is_pushed_once_the_person_leaves_it`; the
    existing `needs_you_pushes_once_per_ask` now expects the push when the desk is left.

- ✅ **A remote Mac's Screen Recording grant has a door from here** (2026-10-12, readiness
  10-12 rank 20, the app's half). A running worker never sees a grant made at its desk, and
  only this Mac's worker was ever restarted, so a grant on another Mac did nothing until
  someone restarted its worker by hand. ⌘O said only that Screen Recording was off.
  - Every place that meets the refusal now says the tile's own words (`failure_text`, "may
    not record its screen. Turn on Screen Recording for slopty-worker in its System
    Settings.") and offers "Restart Slopty there", which sends `Verb::RestartWorker`. The
    worker exits for launchd to start it again, and its shells stay with ptyd. ⌘O on a Mac
    that cannot capture, a window list refused and an open refused all raise one notice per
    machine. It stays until it is pressed or dismissed, because the grant is made at the
    other Mac first. A tile whose window was refused offers the restart in its pane in place
    of "Choose another window", which would be refused the same way.
  - ⌘O's own second wording ("can't share its screen: Screen Recording is off") is deleted.
    A Linux worker with no capture says the tile's "has no screen to share".
  - The words say "Slopty", not "worker": the chrome names a computer a machine, and
    what restarts is Slopty's service on it.
  - Tests: `workspace::tests::facts::a_workers_health_shows_only_when_something_is_wrong`,
    `workspace::tests::bodies::a_window_refused_for_screen_recording_offers_the_restart`
    (slopty-ui).

- ✅ **A pocketed phone hears its notes by push alone** (2026-10-12, readiness 10-12
  deletions). It replaces "Five seconds before the background grace runs out … it says
  `listening: false`" in "Notes reach a pocketed phone".
  - **Why.** Through the grace the phone still heard notices on its link and posted them as
    local notes. That made a second path beside the push. A note heard that way was never
    recorded for take-back, so it stayed on the phone with live buttons after the person
    answered at the Mac (item 22).
  - **Now.** The moment the app leaves the front it says `listening: false` (`set_listening`,
    sent with the presence the window gives at once). The server pushes from then on, and it
    takes its pushes back. The grace is still begun and held until the app returns, but only
    so the links stay up for transfers under way. The countdown that read the grace's time
    left (`until_deaf`, `STOP_BEFORE`, `LOOK_AGAIN`, `BackgroundGrace::remaining`) is deleted.
  - Not covered by a host test: the code is iOS-only. `Attention::set_listening` keeps its
    own tests, and the simulator lane exercises a pocketed phone.

- ✅ **A machine the tailnet's policy turns away is offered its grant** (2026-10-12,
  readiness 10-12 rank 14, the app's half). A tagged node is owned by nobody, so it needs a
  grant of the worker role. The app only ever built client grants, and a failed deploy sent
  the person to `[server] allow`, which cannot let such a node in.
  - The worker now says `LinkState::NotGranted` (lane W). A deploy whose machine runs but is
    never listed, and whose health says so, fails as "The tailnet policy does not let <host>
    in as a machine". The hint names the palette's "Copy the tailnet grant for Slopty's
    machines" and where to paste it, and no longer mentions the allow list.
  - That line copies `worker_grant()`: from `tag:slopty-worker` to a server tagged as
    discovery prefers it, or to one a member owns (`autogroup:member`), with the worker role.
    This Mac's checklist says the same on its server line, with "Copy the grant" as its button.
  - Tests: `server::tests::the_worker_grant_lets_a_tagged_machine_in`,
    `this_mac::tests::the_server_line_follows_the_server_then_the_workers_link` (slopty-app).

- ✅ **A press on a killed app is proved on the simulator** (2026-10-12, readiness 10-12
  rank 7). The windowless answer (`verdict::answer_unheard`) had host tests for its parts and
  a Mac scenario through `press_note`, but nothing launched the phone app cold from a press.
  - simctl delivers pushes but cannot press a note's button. The e2e build reads
    `SLOPTY_E2E_LAUNCH_TAP` (a JSON press) and, right after `notify::install` in
    `didFinishLaunching`, hands it to `notify::deliver_launching`, the same `deliver` the
    system's launching response reaches. Nothing listens yet, so the press takes the
    `answer_unheard` path exactly as a real one would. The hook sits behind the `e2e` feature,
    so no release build carries it.
  - `Stack::relaunch_on_simulator(env)` ends the app as the system ends a suspended one and
    launches it again on the same data with the press.
  - Test: `a_killed_app_answers_a_note_s_deny_with_no_window` (`tests/ios.rs`, live by
    `cargo xtask e2e ios`). A held Bash prompt is denied by a cold launch, the relay ends
    well and the thread's request is gone; the same press again shows "That prompt is no
    longer waiting" in the pressed note's place. It prints a `MEASURE ios-cold` line.

- ✅ **A Mac the person left counts as left at once** (2026-10-12, readiness 10-12 rank 1,
  the app's half; the server's half is "A need told at the desk reaches the phone once the
  person leaves it" above). The Mac counted as active until two minutes passed with no input.
  That was the only sign of leaving, so a need in those first minutes went to the desk alone.
  - The Mac going to sleep, its screens sleeping (display sleep, or the lid closed on a Mac
    kept running), its login session giving way to another, and its screen locking each count
    as left at once (`slopty_platform::away::Left`, from AppKit's workspace and distributed
    notifications). The idle timeout stays for a person who simply walks off.
  - Each way of leaving lasts until its own way back (`Left::ended_by`): woke, screens woke,
    the session back, unlocked. Screens that wake on a locked Mac end only the sleep, so the
    Mac stays left until it is unlocked. An iPhone or iPad needs none of this, because the
    system resigns the app when its screen locks.
  - Tests: `slopty-platform` `away::tests::each_leaving_ends_by_its_own_way_back`;
    `slopty-app` `presence::tests::a_leaving_lasts_until_its_own_way_back`.

- ✅ **A phone asks for notes once it reaches the server** (2026-10-12, readiness 10-12
  rank 4). The phone registered for pushes only once alerts were allowed, and on iOS nothing
  asked until the first note posted, which happened only in the background. So the server
  knew no phone, held no prompt for one and pushed nothing.
  - Where pushes are how an agent reaches the person (a phone, an iPad), the navigator
    carries a line under the server's while notes do not reach them. Never asked, it reads
    "Notify me when an agent needs me" with Allow, the system's question, asked once while it
    is up. Turned off, it reads "Notifications are off" with "Turn on", which opens the
    system's settings. Under either: "Nothing reaches you here while Slopty is away."
  - The answer goes to the navigator and to the server, which may push to this phone from
    then on. The state is read again once the server links and on each return to the front,
    since the person may change it in Settings meanwhile.
  - Test: `workspace::tests::setup_doors::a_phone_s_navigator_asks_for_notes_until_they_reach_the_person`
    (slopty-ui).

- ✅ **A tapped note waits for its machine, and a press that did not land says so**
  (2026-10-12, readiness 10-12 rank 7). On a cold launch, a tap was refused as "not
  reachable from here" while its worker's link was still dialling, and never tried again. An
  Allow or Deny pressed with no window that failed only logged, so the person believed it
  went.
  - A tap whose tile this device does not have yet waits for its machine to link and send
    what it holds, saying "Connecting to <machine>…" meanwhile, then goes where it leads. It
    waits at most 30 s (`attention::PARKED_FOR`): a link slower than that is not what the
    person is still waiting on. A tap on a tile already here needs no link.
  - A background answer other than sent posts a note in place of the pressed one
    (`verdict::missed_note`): the machine could not be reached, named as this device last
    knew it, or "That prompt is no longer waiting" ("It was answered elsewhere, or it ended.").
    The note keeps the pressed one's way to the agent without its request, so a tap shows the
    agent and no button answers twice. A reply that did not go keeps its field.
  - Tests: `workspace::tests::attention::a_note_tapped_before_its_machine_links_goes_there_once_it_does`
    (slopty-ui); `verdict::tests::a_background_answer_that_did_not_land_is_said` (slopty-app);
    the simulator's cold press is "A press on a killed app is proved on the simulator" above.

- ✅ **A small question is answered from its note** (2026-10-12, readiness 10-12 item 13, the
  wire and the server). A note offered Allow and Deny on a plain yes or no and nothing else, so
  a question an agent asked could only be opened.
  - A request card carries `buttons` (`NoteChoice`), made where the card is
    (`NoteChoice::of`): for a question that asks one thing, takes one answer and offers at
    most four (what a notification's actions show), one per option, each with the choice
    that answers the request with it alone, as the question's own dialog would send it. Any
    other request has none, and is opened or replied to.
  - The server pushes a needing thread's first request with its note when it is a yes or no
    Allow and Deny answer, or when it has buttons. `PushBody.choices` carries them, and the
    phone offers them as the note's actions, each answering `ask` with its choice.
  - Tests: `slopty-proto` `units::a_small_question_is_answered_by_its_options`, the
    `push_body_choices` golden; `slopty-server`
    `hub::ladder::tests::needs_you_pushes_once_per_ask` (the pocketed phone's push carries a
    question's buttons).

- ✅ **A note answers what its agent asks, not only a yes or no taken at first push**
  (2026-10-12, readiness 10-12 rank 13, the app's half; the server re-pushes quietly when the
  request moves and carries the options, lane W).
  - **Replies.** An agent's note with no yes or no (a question, a plan, a failure, a finish)
    carries a Reply field (`notify::REPLYING`). The reply goes to the thread as a message:
    straight to a worker linked here as the thread's composer sends it, else through the
    server (`Verb::SendMessage`), from a killed app's background answer too. A reply that did
    not go is said on a note that keeps the field.
  - **Options as buttons.** A question that asks one thing, takes one answer and offers at
    most four puts its options on the note in place of Allow and Deny (`Note::picking`,
    `pick.0` on). Each answers the request with the choice the note keeps for it under the
    button's id (`notify::Pressed`). Notification categories are fixed sets of buttons, so a
    category is made for each set of labels (`picking_id`, an FNV-1a hash, so the same
    options share one). It is registered as the note goes out: by the app for its own notes,
    and by the notification extension before it hands back a pushed one (`register_then`).
  - **One set for both processes.** The centre keeps one category set for the app and its
    extension, and setting it replaces all of it. So each registration reads the set first
    and keeps the picking categories already there, up to 64; the app's launch keeps them too.
    The note is added, or handed back, only once a read after the set lists it, so its
    buttons are there when it shows. If the centre never answers, the system shows the push
    as it came once the extension's time is up.
  - **Answering.** A pick is answered where the note is, as Allow is (`Tap::finished_later`):
    with the workspace when it is up, and only while the open request offers that choice;
    else with no window, through the server. One that did not land says "Your answer was not
    sent", with no pick left to press twice.
  - Tests: `notify::pushed::tests::a_questions_options_are_its_notes_buttons` (slopty-platform);
    `verdict::tests::a_pick_answers_with_its_own_choice_with_no_window` (slopty-app);
    `workspace::tests::attention::a_small_question_is_answered_from_its_notes_options`,
    `workspace::tests::approvals::a_notes_reply_goes_straight_to_a_linked_worker`,
    `…::a_notes_reply_goes_through_the_server_while_its_worker_is_away` (slopty-ui).

- ✅ **A pushed note keeps up with its thread** (2026-10-11, orchestrator-first study item 23,
  the server's half). Three ways a pocketed phone's note fell behind what it said:
  - **The request moved under it.** A thread that still needed the person while its request
    changed (a new one opened in the old one's place, or the one shown went) was never pushed
    again, so its buttons answered a request that was gone. Each phone's note now remembers
    the request its buttons answer (`Shown::asks`). Every ranking pass compares that request
    with the thread's current one, for threads that needed the person at the last pass too
    (`ladder::follow_asks`). When they differ, the note is pushed again with `quiet` set, under
    the same collapse id, with the new request's words and buttons and no sound
    (`Phones::follow`). What the buttons answer is kept only in memory. A server started again
    moves each shown note once, quietly, since it cannot know what the note's buttons answer.
  - **A need first seen.** A thread was told only when its rung moved from a known one, so a
    need first seen after a server restart, or after its worker linked again, was never
    pushed. Now it counts (`Told::first`). It is pushed only when the person is at no client,
    and only to a phone not already showing it, as when they leave the desk. No link is told,
    because the clients that held it still do.
  - **A finished turn already read.** A Finished note stayed on the phone after the person
    read the turn on the Mac. The note now records its turn (`Shown::finished`, kept in
    `push.json`). It is taken back once the row's `seen` reaches that turn, or once the thread
    is gone from a linked worker's table. A later note about the thread replaces it, and with
    it the record. The file's shape changed again: an old one is set aside.
  - Tests: `slopty-server` `hub::ladder::tests::a_shown_need_follows_its_request_quietly`,
    `…::a_need_first_seen_is_pushed_to_a_phone_not_showing_it`,
    `…::a_finished_note_is_taken_back_once_its_turn_is_seen`.

- ✅ **Save to Files comes down into the folder chosen** (2026-10-10, readiness audit "below
  the line"). On iPhone and iPad, "Save to Files…" and "Save a copy…" brought the whole file
  into the app's outbox before the export sheet showed, with no progress and no stop, and the
  file was stored twice. A large file looked like nothing happening.
  - **Where first.** The Files picker opens on folders, not copies
    (`file_drop::picker::choose_folder`, `UIDocumentPickerMode::Open` on `public.folder`).
    The worker's file then comes down straight into the folder chosen, as any download does on
    the Mac. It is listed in the transfers popover with its progress, its rate and its stop. It
    lands in a hidden staging directory beside its name and is renamed into place, under a name
    the folder does not have yet (`name (2).ext`).
  - **The folder's scope.** A folder outside the sandbox is reached only inside its security
    scope. `picker::Scoped` enters it when the folder is chosen and leaves it when dropped, one
    stop for the one start that succeeded. The download holds it until it stops writing, which
    covers landing, failure, the person's stop, a lost link and the view going.
  - **Not resumed after a relaunch.** The grant is not bookmarked, so such a download is not
    kept in the transfer ledger; the next run would not hold the folder.
  - Test: `workspace::tests::remote::saving_to_files_comes_down_into_the_chosen_folder`
    (slopty-ui, on the Mac through the same `save_into`).

- ✅ **A note may say work is ready to merge or a goal is met, and carry the task its Merge
  merges** (2026-10-10, the orchestrator-first study, items 12 and 13, the wire half).
  - `NoticeKind::ReadyToMerge` is one note per project, its text the count of tasks ready.
    `PushBody.merges` names the task its Merge action merges, the oldest ready one, so the
    phone acts without a round trip to learn it. `NoticeKind::GoalDone` is the orchestrator
    saying its goal is met, once (`Verb::ProjectProgress { done: true }`).
  - This change carries the wire alone. The server raises neither yet, and the notes'
    categories and the Merge action are items 12 and 13. The ladder words both kinds as it
    words a finished one, from the row's last line.
  - Goldens: `push_body*` (each body gains `merges`) and `push_body_ready_to_merge`.


- ✅ **The push relay is deleted** (2026-10-10, orchestrator-first audit §C.5). There were two
  ways to APNs: a Cloudflare Worker the person deployed (`apps/slopty-relay`), which held the
  team's APNs key, and the person's own `.p8` named in `[server.push]`. One way is enough. The
  phone app reaches people through TestFlight, which already needs the Apple developer account
  the key comes from, and the key path adds no service to deploy or keep running.
  - **Gone.** `apps/slopty-relay`; `slopty_push::relay` and its Ed25519 signing; the
    server's `RelayPusher`, `PushConfig::Relay` and the install key in `push.key`;
    `[server.push] relay` and its row in the form; the wasm32 clippy step, target and CI
    target. `slopty-push` always has a random source now, so it has no `getrandom` feature.
  - **Kept.** `[server.push] apns_key`, `key_id` and `team_id`, the sealed body, the provider
    token and the take-back, all unchanged. A file that still names `relay` loads with an
    unknown-key warning, and notes stay off until a key is named.
  - **This Mac's line** says "Off until you name your APNs key", and Set up opens the form on
    that field.
  - Tests: `tests/push.rs` `a_notice_reaches_the_phone_and_is_taken_back` (slopty-server),
    `notes_reach_a_phone_as_server_push_says` (slopty-serverd),
    `this_mac::tests::a_server_here_says_how_notes_reach_a_phone` (slopty-app).

- ✅ **Merge on a note of work ready to merge** (2026-10-11, the orchestrator-first study, item
  13, the app's half).
  - A pushed `ReadyToMerge` note whose body names a task (`PushBody.merges`) carries the
    `slopty.merge` category: "Merge", answered where the note is (on an unlocked device), and
    "Show". The note keeps the project and the task in its `userInfo` (`info::PROJECT`,
    `info::TASK`), so a press merges with no round trip to learn which. A note naming no task
    has no buttons.
  - A press is `Verb::TaskMerge`, the person's word, as the board's Merge is. With the app
    running it goes through the workspace's server link and settles the tap once answered; a
    refusal is a toast in the server's words. With no window (an iOS press that launched or woke
    the app) `verdict::answer_alone` sends it on a link of its own. A task merged already or
    gone is said quietly in place of the note ("That work no longer waits to merge"); one that
    did not reach the server keeps its Merge, to press again.
  - Tests: `notify::pushed::tests::a_ready_note_merges_the_task_it_names` (slopty-platform),
    `verdict::tests::a_merge_pressed_on_a_ready_note_merges_its_task` (slopty-app),
    `workspace::tests::approvals::a_ready_notes_merge_merges_the_task_it_names` (slopty-ui).
