# Decisions — Platform

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **Floor macOS 26.5 / iOS 26.5, Apple silicon only.** User decision 2026-09-04. No
  availability checks, no fallbacks for older OS.

- ✅ **Pure Rust.** Scripts are `xtask`. The only non-Rust files are the iOS `main.m` shim (a
  UIKit bootstrap that can't be avoided until `UIApplicationMain` is driven from Rust in the GPUI
  fork), Metal shaders, and the XcodeGen spec that xtask generates.

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
