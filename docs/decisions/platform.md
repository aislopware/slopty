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
    - The clipboard is `slopty_input::pasteboard::Unsupported`: it never changes and refuses
      every write. Off macOS, `Rep::uti` spells the UTIs the wire carries; a Mac test holds the
      spelling to AppKit's statics.
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
    `LINUX_UNTESTED` keeps off the lane are not covered either.

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
    and ends while the app is away notifies too, with the command and how it ended. An agent
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
  - Open: the workspace still posts its own agent banners through GPUI
    (`WorkspaceView::notify_system`, `notify_program`), so on macOS an agent's note goes out
    twice under one identifier and the second replaces the first. `command_finished` counts the
    focused tile as watched even while the app is away, so that command notifies but does not
    enter the inbox or the badge.

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
    elsewhere, and `format_of` reads a board's type back. There is no `Other(String)`, because
    the sync offers only formats it knows how to write, so an unknown one has nowhere to go.
  - **A display is a `DisplayId`**, a newtype in `slopty-core`, not a bare `u32` that happened
    to be a CoreGraphics id. `server::DisplayCap` duplicated `screen::DisplayInfo` and is gone,
    so `WorkerCaps.displays` carries `DisplayInfo`.
  - **`Os` has no default.** A worker says what it runs on. `WorkerCaps::bare(os)` is the
    fixture constructor.
  - **The shell fallback is `/bin/sh`** when `SHELL` is unset, not `/bin/zsh`, which a Linux
    worker may not have.
  - Goldens: `client_clip_offer`, `worker_clip_data` and `worker_clip_fetch`. Tests:
    `each_format_is_an_apple_uti_and_back` (`slopty-input`).
