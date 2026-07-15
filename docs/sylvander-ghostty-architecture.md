# Sylvander-Ghostty Architecture

> A deep-dive into how `sylvander-ghostty/` works — what it does,
> why it's structured the way it is, where every hook lives, and
> where our F2-F6 work is supposed to plug in.

> **Product direction update (2026-07-11):**
> [`sylvander-tui-ux-design.md`](./sylvander-tui-ux-design.md) is the
> source of truth for the agent conversation experience. Ghostty hosts one
> session-bound `sylvander-tui` process per retained PTY surface and presents
> those surfaces through a native left session rail, never top session tabs. The earlier
> native Swift agent-workbench direction in this document is retained
> only as historical architecture analysis and must not drive new UI
> implementation.

### Desktop host capability boundary

The macOS host may expose local presentation capabilities that portable TUI
clients do not have. Each TUI surface receives a process-private Unix socket
path and a 256-bit random token bound to its server `SessionId`; requests use a
bounded, versioned JSON line and never travel through the Agent service.

- `/preview image <path>` resolves symlinks and accepts only regular supported
  images inside that session's workspace, up to 25 MiB.
- `/preview web <url>` accepts public HTTPS URLs and local development HTTP
  URLs without embedded credentials. The Inspector defaults to the system
  browser for JavaScript, authentication, localhost, and developer tools. An
  explicit Quick Look uses a non-persistent `WKWebView`, disables JavaScript,
  and rejects cross-origin top-level navigation.
- Unknown sessions, mismatched tokens, oversized frames, local/private literal
  addresses, and malformed requests fail closed. Agent output never auto-opens
  a preview; a user must invoke the local preview command.

### Embedded TUI helper

The macOS product embeds `sylvander-tui` at
`Contents/Resources/bin/sylvander-tui`. `macos/build.nu` builds the helper first
and passes its absolute path to Xcode. The `Embed Sylvander TUI` phase rejects a
missing or non-executable helper, verifies every architecture in `ARCHS`,
installs it with executable permissions, and signs it with the app identity.

`SYLVANDER_TUI_PATH` remains an explicit development and CI override. Runtime
launch prefers this override and otherwise resolves only the bundled helper.

Tag releases additionally require `MACOS_CERTIFICATE_P12`,
`MACOS_CERTIFICATE_PASSWORD`, `MACOS_SIGNING_IDENTITY`, `APPLE_ID`,
`APPLE_TEAM_ID`, and `APPLE_APP_PASSWORD` repository secrets. The release job
creates a two-architecture Archive, verifies its nested signatures, submits it
to Apple's notary service, staples and assesses the ticket, then publishes a
draft zip with a SHA-256 checksum. Missing credentials fail the release before
any distributable artifact is created.

Last audited against the **`b14d92383`** upstream Ghostty commit
embedded in this repo (F1.12). Update this file if you re-sync
(`git subtree pull`) and the surface area changes.

This document assumes you have read `sylvander-ghostty/SYNCUP.md`.
That document explains the **fork mechanics**; this one explains
**what's inside the fork**.

## 1. what Ghostty does (functions)

A GPU-accelerated terminal emulator available on macOS, Linux,
FreeBSD, Windows, and WASM. Concretely:

| Surface | Capability |
|---|---|
| **Terminal emulation** | PTY byte stream → Unicode + control sequences (CSI/OSC/VT100 escape) → grid render |
| **Graphics protocols** | Kitty graphics, sixel, iTerm2 inline images |
| **Color** | 256-color and 24-bit true-color, dynamic palette via OSC |
| **Typography** | Ligatures, fallback fonts, per-cell shaping; configurable via font config |
| **Rendering backends** | Metal (macOS), OpenGL (Linux/Win), WebGL (WASM) — picked at comptime |
| **App shell** | Tab management, window management, TOML config, key bindings, command palette, search, hyperlinks, clipboards, IME |
| **Native UIs** | macOS (AppKit + AppleScript + Sparkle auto-update); Linux/BSD (GTK 4 + libadwaita) |
| **Library form** | Embeddable as `libghostty-vt` (VT only) or `GhosttyKit.xcframework` (full C API) |

The piece we're pivoting it into: **a first-class Sylvander AI
agent frontend**, where a new tab kind (not driven by a PTY but
by a Sylvander server connection) renders an AI workbench UI.

## 2. layered architecture

```
┌───────────────────────────────────────────────────────────────────┐
│ Layer 0: comptime config — BuildConfig (src/build/Config.zig)     │
│   Drives all of: app_runtime, renderer, font_backend, flatpak,   │
│   snap, app_version, target, optimize, …                          │
├───────────────────────────────────────────────────────────────────┤
│ Layer 1: app runtime — src/apprt.zig → runtime = {                 │
│   embedded  (macOS: Swift ↔ C ABI ↔ Zig via GhosttyKit)            │
│   gtk       (Linux/BSD: GObject + GTK 4)                          │
│   browser   (WASM target)                                         │
│ }                                                                  │
├───────────────────────────────────────────────────────────────────┤
│ Layer 2: core surface — src/Surface.zig (6 036 lines, single)     │
│   - keyboard, mouse, selection, clipboard                          │
│   - terminal grid → renderer.drawFrame                             │
│   - owns termio thread + renderer thread + search thread           │
├───────────────────────────────────────────────────────────────────┤
│ Layer 3: IO — src/termio/                                         │
│   - PTY backend (only Kind=exec today)                             │
│   - xev.Loop on the IO thread                                      │
│   - SPSC mailbox for cross-thread messages                        │
├───────────────────────────────────────────────────────────────────┤
│ Layer 4: renderer — src/renderer/                                  │
│   - GenericRenderer(comptime GraphicsAPI)                          │
│   - 8 fixed passes: bg → image-bg → cell-bg → text → image →     │
│     cursor → overlay → custom-shader                               │
└───────────────────────────────────────────────────────────────────┘
```

A given build picks exactly one runtime, one renderer backend,
one IO backend at **compile time**. There is no runtime
polymorphism on the hot path.

## 3. the Swift ↔ Zig contract (the only external surface)

> Everything outside `GhosttyKit.xcframework` talks to the C ABI
> in `include/ghostty.h`. Anything inside the xcframework is fair
> game to refactor as long as the public symbols stay put.

```
                  ┌─────────────────────────────────────┐
   Swift calls ──►│  ghostty.h (1209 lines C header)     │
                  │  (umbrella: module.modulemap)        │
                  └──────────────┬──────────────────────┘
                                 │ @_silgen_name / extern "C"
                  ┌──────────────▼──────────────────────┐
                  │  src/main_c.zig → embedded.zig CAPI  │
                  │  ghostty_init, ghostty_app_new,    │
                  │  ghostty_surface_new, …              │
                  └──────────────┬──────────────────────┘
                                 │ typed function pointers
                  ┌──────────────▼──────────────────────┐
                  │  ghostty_runtime_config_s           │
                  │  wakeup_cb / action_cb /            │
                  │  read_clipboard_cb / close_surface_cb│
                  └─────────────────────────────────────┘
                                 ▲ set at App init time
                                 │ (Ghostty.App.swift:64)
                                 │
                    Swift ━━━━━━━┘
```

### Tracing one action end-to-end

User presses the new-tab key combo:

1. Swift `Ghostty.App.newTab(surface:)` calls
   `ghostty_surface_binding_action(surface, "new_tab", len)`
   (`macos/Sources/Ghostty/Ghostty.App.swift:190-195`)
2. Zig `embedded.zig:1987` parses the action, calls
   `core_surface.performBindingAction(...)` on the affected surface
3. Zig `Surface.zig:4775` checks the action's scope. `new_tab` is
   app-scoped, so forwards to `app.performAction(...)`
4. Zig `App.zig:417` calls `rt_app.performAction(.app, .new_tab, {})`
5. Zig `embedded.zig:267-287` finally pushes through to
   `self.opts.action(...)` — the `action_cb` registered by Swift
6. Swift `App.action(...)` (`Ghostty.App.swift:481-685`) switches
   on the `tag` field; `.new_tab` posts a `Notification` to
   `NotificationCenter`; `AppDelegate` listens and creates the
   new tab.

Every action needs to be registered **in three places** to be a
Swift-visible tab kind:

| Where | Why |
|---|---|
| `src/apprt/action.zig:351` (`Key` enum) | The Zig identifier |
| `include/ghostty.h:886-952` (`GHOSTTY_ACTION_*` enum) | The C ABI tag |
| `Ghostty.App.swift:481-685` (`App.action`) | The Swift switch case |

## 4. three threads per surface

| Thread | Entry | Event loop | File |
|---|---|---|---|
| **IO** | `termio.Thread.threadMain()` | `xev.Loop` | `src/termio/Thread.zig:135` |
| **Renderer** | `renderer.Thread.threadMain()` | `xev.Loop` | `src/renderer/Thread.zig` |
| **Search** | `terminal.search.Thread` | custom | `src/terminal/search/` |

The **main thread** is driven by Cocoa (NSApplication event loop);
Zig work happens via `ghostty_app_tick()` (`embedded.zig:1427`)
invoked from `wakeup_cb` whenever the App mailbox wakes the app
thread.

### Message flow

```
                           app thread (Cocoa)
                           ┌──────────────────────┐
                           │ App mailbox (BQueue64)│◄─┐ drainMailbox
                           └──────────┬───────────┘  │ tick()
                                      │              │
   ┌───────────────┐  surface_message ┼───┐          │
   │ surface.Mail │ ─────────────────►│   │          │
   └───────────────┘                  │   │          │
                                      │   │          │
   ┌───────────────┐  termio.Message  ┼───┼─►action_cb│
   │ termio.BQueue│ ───────────────►wakeup─┘          │
   └───────────────┘                                 │
   (IO thread)                                       ▼
                                                Swift UI
```

`wakeup_cb` uses `DispatchQueue.main.async` (`Ghostty.App.swift:434-441`)
so the mailbox callback hop is always onto the main actor.

## 5. comptime config

Driver chain:

```
build.zig ──► src/build/Config.zig:72  (50+ b.option() decls)
          └─► src/build/Config.zig:526 (step.addOption for each)
             └─► src/build_config.zig:37-43  (pub const re-export)

Consumers:
  comptime build_config.flatpak  → src/apprt/gtk/flatpak.zig:8
  comptime build_config.snap     → src/apprt/gtk/class/surface.zig:1621
  comptime !build_config.flatpak → src/os/passwd.zig:58
```

We have already patched `bundle_id` (`src/build_config.zig:58`)
from `com.mitchellh.ghostty` to `ai.oraculo.sylvander`. This is
used by:

| File | Line | Purpose |
|---|---|---|
| `src/main_ghostty.zig` | 142 | macOS `os_log` subsystem |
| `src/benchmark/Benchmark.zig` | 51 | Identifier |
| `src/apprt/gtk/class/inspector_window.zig` | 92 | Icon name |
| `src/apprt/gtk/class/window.zig` | 326 | Icon name |
| `src/os/macos.zig` | 31, 46 | Application Support / Cache paths |
| `src/os/i18n.zig` | 47-67 | gettext domain binding |
| `macos/Sylvander-Info.plist` | 115 | `ai.oraculo.sylvander` (duplicate, hardcoded) |
| `macos/Sylvander.xcodeproj/project.pbxproj` | 783+ | `PRODUCT_BUNDLE_IDENTIFIER` (also hardcoded) |

The last two are set in two places because `PRODUCT_BUNDLE_IDENTIFIER`
is an Xcode build setting, not a Zig comptime value; we patch
both deliberately for F1.14.

### Pattern: adding a new comptime flag (e.g. `sylvander_enabled`)

Following the `flatpak` precedent, the surface area is:

1. `src/build/Config.zig` (struct field)
2. `src/build/Config.zig` (`b.option` declaration in `init()`)
3. `src/build/Config.zig` (`step.addOption` in `addOptions()`)
4. `src/build/Config.zig` (`.sylvander_enabled = options.sylvander_enabled` in `fromOptions()`)
5. `src/build_config.zig` (`pub const sylvander_enabled = …`)
6. Consumer: `if (comptime build_config.sylvander_enabled) { … }`

## 6. build pipeline

```
build.zig:37   ─►  Config.init(b, target)               [from src/build/Config.zig]
        :49   ─►  SharedDeps.init(b, config)           [Step that adds C deps & atomic emit paths]
        :53   ─►  GhosttyZig.init(b, deps, cfg)        [creates the Zig module for ghostty-vt]
        :85   ─►  GhosttyExe.init(b, deps, cfg)        [addExecutable name="ghostty" root=src/main.zig]
        :117  ─►  GhosttyLibVt.initShared/Wasm         [libghostty-vt.so/.dylib/...]
        :133  ─►  GhosttyLibVt.initStatic             [libghostty-vt.a]
        :158  ─►  GhosttyLibVt.initStaticAppleUniversal + xcframework (lib-vt)
        :189  ─►  GhosttyLib.initShared                [libghostty.so/.dylib]
        :213  ─►  GhosttyXCFramework.init              [GhosttyKit.xcframework]
        :228  ─►  GhosttyXcodebuild.init                [xcodebuild → .app]
```

### `GhosttyKit.xcframework` — what Swift actually links

The `.xcframework` is the binary Swift imports. It bundles:

| Slice | Source |
|---|---|
| `macos-arm64_x86_64` | `GhosttyLib.initMacOSUniversal` → combined `libghostty-internal.a` |
| `ios-arm64` | `GhosttyLib.initStatic(aarch64)` |
| `ios-arm64-simulator` | `GhosttyLib.initStatic(simulator)` |

The umbrella header `macos/GhosttyKit.xcframework/.../Headers/ghostty.h`
is the only thing Swift code can call. Anything else (Zig objects,
GPU shaders, C helpers outside the header) is invisible.

### `src/sylvander/` — where Sylvander goes

```
sylvander-ghostty/src/sylvander/
├── mod.zig            (barrel — @imports submodules, pub const re-exports)
├── build.zig          (standalone build target — used by CI zig-module job)
├── build.zig.zon      (its own package — does NOT need to be in main .zon)
├── connection.zig     (WSS / Unix socket client)
├── session.zig        (per-session state machine)
├── event.zig          (wire-format types — JSON tagged-union)
├── config.zig         (user-facing settings + comptime toggles)
└── protocol.zig       (framing rules)
```

The directory is currently empty (F1.12 reverted the skeleton). It
can compile in two ways:

- **Standalone** via its own `build.zig` (CI `zig-module` job). Tests
  pass cleanly in isolation, without needing the full Ghostty tree.
- **Integrated** by registering it as a Zig `addModule` from the
  parent `build.zig`, then `step.root_module.addImport("sylvander", …)`
  in `GhosttyLib.zig`. Swift never directly imports `src/sylvander/`;
  only the public C API does.

## 7. F2-F6 hook plan

This is **why we are here**. The architectural decision is:

> **A Sylvander tab is *not* a CoreSurface variant**. CoreSurface
> has a hard invariant that `init()` always allocates a PTY (`Surface.zig:649`).
> We will not relax this — it would force downstream grid-rendering
> assumptions into workbench UI code, and would have cascading effects
> on 7+ files in termio init, threads, env, and child-process
> management.

Instead, the Sylvander tab lives entirely in the **apprt** layer,
parallel to the existing Terminal tab kind:

| Phase | Where | What |
|---|---|---|
| **F2** | macOS Swift (`macos/Sources/Features/Sylvander/`) + GTK Zig (`src/apprt/gtk/class/sylvander_tab.zig`) | New `SylvanderController`/`SylvanderTab` GObject; an `NSWindowController` that renders a SwiftUI / Metal UI, never calls `ghostty_surface_new` |
| **F3** | `src/sylvander/` (the directory above) + new termio `Kind` (optional) | Connection / Session via xev + std.http; runs on its own xev.Loop parallel to IO/Renderer threads |
| **F4** | Same as F2 — UI layer | Sidebar + chat + input + status rendered by the new view, fed by F3 events |
| **F5** | Reuse `Surface.showDesktopNotification` (`Surface.zig:5968-6012`) → Swift `UNUserNotificationCenter` | Native macOS alerts (already wired) |
| **F6** | Server-side, not in this codebase — Sylvander server handles session persistence | We mirror server-side state into the tab |

### Concrete files to touch for F2

| File | Change |
|---|---|
| `src/config/Config.zig` | `+sylvander_enabled: bool = false` (and the 5 other touchpoints from §5) |
| `src/apprt/action.zig:351` (`Key` enum) | `+new_sylvander_tab` |
| `macos/Sources/Features/Sylvander/SylvanderController.swift` | New `NSWindowController` + `NSViewController` hosting a SwiftUI or Metal view |
| `macos/Sources/App/macOS/AppDelegate.swift:481-685` (`App.action`) | `+case .newSylvanderTab: SylvanderController(ghostty).showWindow(self)` |
| `macos/Sources/Features/Tab/Tab.swift` (or analogous) | Optional: surface a "+ Sylvander" button in the tab bar |
| `src/apprt/gtk/class/sylvander_tab.zig` (new) | GObject equivalent for GTK |
| `src/apprt/gtk/App.zig` | Handle `new_sylvander_tab` in the GTK dispatcher |

### Why this is "forking"-friendly

- **Upstream-safe**: none of these edits move upstream files in
  a way that conflicts with `git subtree pull`. Add new files
  under `macos/Sources/Features/Sylvander/`, `src/apprt/gtk/class/`,
  and `src/sylvander/`. Touch upstream files (Config.zig,
  action.zig, AppDelegate.swift) only to add cases or methods.
- **Rebrand-aware**: the `SylvanderKit` framework name (and
  Swift `import Ghostty`) is intentional. Renaming it would
  cascade through every Swift file. The Sylvander *display*
  brand we have (F1.14) is enough.
- **Boundary preservation**: any feature regression in core
  Ghostty (e.g. a Panther X server fix) propagates via
  `git subtree pull` without crossing into Sylvander territory,
  because our patches are additive — new files, new enum cases,
  new actions — not rewrites.

## 8. cross-cutting concerns

### Network (intentionally absent at F1)

There is **zero** network code in `sylvander-ghostty/src/`. Even
basic `std.http`/`tls.Client` lookups return no matches. The
closest analog is PTY handling under `src/termio/`, which has
the same shape as a socket read handler (registered on xev.Loop,
notifies via mailbox, surfaces errors as `apprt.surface.Message`
variants). F3's `Connection` will follow that pattern, with its
own xev loop running in a new task thread rather than the existing
IO thread (to avoid competing with PTY IO if both ever coexist).

### Threading model

- Every long-running async task is an `xev.Loop` running on its
  own OS thread.
- Cross-thread communication is `BoundedQueue(Mailbox, 64)` +
  `xev.Async` wakeup. There is no shared mutable state outside
  these queues and the rendered `Surface`'s render thread mutex.
- The main thread is owned by Cocoa; Zig never blocks it.

### Logging and observability

- Zig has its own `log.scoped(...)` infrastructure
  (`src/log.zig`). Scoped loggers are wired in
  `src/main_ghostty.zig` and friends using `bundle_id` for the
  `os_log` subsystem on macOS (after F1.14, the subsystem is
  `ai.oraculo.sylvander`).
- Rate-limited desktop notifications already in place at
  `Surface.zig:5968-6012` (1/sec cap, 5s dedup). F5 reuses
  this path with no new code.

### Performance hot paths

- `drawFrame()` runs on the renderer thread and is allocation-free
  in steady state. F4's UI does **not** live on this path —
  the workbench view is a SwiftUI/MTKView owned by the
  SylvanderController on its own render budget (probably
  `CADisplayLink` for vsync).
- `drainMailbox` runs on the app thread (Cocoa main run loop
  iteration). F2 events land here via `Action` and Swift
  `notificationCenter`, which is internally optimal.

## 9. naming check-list when forking

| Symbol | Status | Why |
|---|---|---|
| `GhosttyKit.xcframework` | keep | Swift `import Ghostty` would need migration |
| `GhosttyScriptTab` (Swift class) | keep | wired through `Sylvander.sdef`'s `<cocoa class=...>` |
| `Ghostty.app` / `ghostty` (binary) | keep | upstream relies on these for helpers in `macos/build.zu` |
| AppleScript 4-char codes (`Ghst`, `Gfst`, `GNTab`, `GWn`, etc.) | keep | registered with macOS at runtime |
| Process name (`argv[0]`) | upstream-keep | SpotLight, Dock activation |
| `bundle_id` (comptime) | **patched** to `ai.oraculo.sylvander` |
| `CFBundleDisplayName` (pbxproj) | **patched** to `Sylvander` |
| `PRODUCT_BUNDLE_IDENTIFIER` (pbxproj) | **patched** to `ai.oraculo.sylvander` |
| Manpage names (`man 1 ghostty`, etc.) | keep | no user need to rename |
| Dist tarball names (`ghostty-X.Y.Z.tar.gz`) | keep | upstream distribution |
| `~/.cache/zig/p/ghostty-1.3.2-…` | keep | matches upstream's `build.zig.zon` `.name`; changing it would invalidate cache and confuse `git subtree pull` |

## 10. how this document was produced

The architectural facts above come from a single multi-agent
deep-dive (3 Explore agents in parallel: tab/surface/renderer,
build system / comptime config, Swift↔Zig bridge / network)
executed 2026-07-10 against `b14d92383` (the upstream HEAD
embedded in this repo). When this repo's `master` moves
forward via `git subtree pull`, re-run the same agent pattern
and diff the result against this file. If something here stops
matching reality, **update this file first**, then implement
against the new state.

## 11. index of important file locations

```
src/build/Config.zig                       comptime option declaration
src/build_config.zig                       comptime re-export & bundle_id (patched)
src/apprt.zig                              runtime selection (none / gtk / browser)
src/apprt/embedded.zig:267                 performAction → action_cb forward
src/apprt/embedded.zig:1399-2250           CAPI (Swift-callable) exports
src/apprt/action.zig:351-422               Key enum for GHOSTTY_ACTION_*
src/apprt/surface.zig:14-155               Surface.Message union
src/apprt/surface.zig:158                  NewSurfaceContext enum (window/tab/split)
src/Surface.zig:62-163                     Core Surface fields
src/Surface.zig:460-680                    Core Surface init
src/Surface.zig:649                        termio.Exec.init (PTY hardcoded)
src/Surface.zig:5968-6012                  showDesktopNotification (F5-reuse)
src/App.zig:527-576                        App.Message union
src/App.zig:562                            App mailbox BlockingQueue(64)
src/App.zig:238-265                        drainMailbox
src/termio/Thread.zig:135                  IO thread entry
src/termio/mailbox.zig                     cross-thread mailbox
src/termio/backend.zig:7-14                Kind = .exec (only)
src/renderer.zig:38                        comptime Renderer dispatch
src/renderer/generic.zig:81                GenericRenderer(comptime API)
src/renderer/generic.zig:1442-1700          drawFrame() (8 passes, all grid)
src/renderer/Options.zig:1-67              Renderer.Options
src/main_c.zig                              C API entry (top-level)
include/ghostty.h                            C API contract
include/module.modulemap                    GhosttyKit module
macos/Sylvander.xcodeproj/project.pbxproj  Xcode project (patched: bundle id + display name)
macos/Sources/Ghostty/Ghostty.App.swift:64  action_cb registration
macos/Sources/Ghostty/Ghostty.App.swift:434 wakeup_cb
macos/Sources/Ghostty/Ghostty.App.swift:481 App.action switch (Swift contract)
macos/Sources/App/macOS/AppDelegate.swift   creates TerminalController on .new_tab
src/sylvander/                              our addition (empty after F1.12)
```
