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

- ⏸ **Attention past the background grace needs push through the server (APNs)**
  (2026-09-28). Local notifications stop when iOS suspends the app, a few tens of seconds
  after it leaves the screen, because the links go with it. Reaching a phone after that needs
  the server to send a push: an APNs key or certificate, the device token sent to the server,
  and a relay the server can reach. The server already hears every agent's state over its
  directory link. Deferred until asked: it brings Apple credentials and a hosted dependency
  into a design that otherwise has none. Its payload would carry the same `userInfo` as the
  local note, so a tap routes the same way.
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
    - *Read-only for now.* Finder refuses a change in the domain. Writing back would need delete,
      rename and make-folder on the wire, which the worker does not offer yet.
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
  - **Not yet.** A folder of more than 2000 entries shows its first 2000 (`FOLDER_ENTRIES`),
    until the listing pages. The fetch's progress moves only at its end.
  - Tests: `files::tests` (slopty-platform: the directory, the roots, one domain per worker and
    none for a forgotten one, and no container for a build the team did not sign),
    `finder::tests` (slopty-app: the switch before a worker's home, and each notice),
    `the_palette_offers_the_workers_in_finder_on_a_mac`, `bundle::tests` (xtask: the
    extension's place in `PlugIns`, its plist and entitlements), `item::tests` and
    `changes::tests` (slopty-files), and `tests/domain.rs` against a real worker daemon: the
    home lists, a deeper item is found, a file's bytes come down with its version, a file made,
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
