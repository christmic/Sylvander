# Sylvander-Ghostty Architecture

> A deep-dive into how `sylvander-ghostty/` works — what it does,
> why it's structured the way it is, where every hook lives, and
> how the native session host embeds the portable TUI.

> **Product direction update (2026-07-11):**
> [`sylvander-tui-ux-design.md`](./sylvander-tui-ux-design.md) is the
> source of truth for the agent conversation experience. Ghostty hosts one
> session-bound `sylvander-tui` process per retained PTY surface and presents
> those surfaces through a native left session rail, never top session tabs.
> Swift owns desktop session management and optional inspectors; the portable
> conversation UI remains the Rust TUI.

### Native session lifecycle

The macOS rail is a management surface for durable server sessions, not a
second session database. It negotiates the public Unix UI protocol for every
operation:

- discovery refreshes every five seconds while online and reconnects with
  bounded exponential backoff after a transport failure;
- creation discovers available Agents first, inherits the selected Agent's
  workspace by default, and sends an override only when the user chooses a
  folder;
- rename, archive, and permanent delete wait for the matching server
  acknowledgement before refreshing local state;
- archive and delete are contextual, confirmed actions. Permanent delete is
  never a primary sidebar control.

Each opened server session owns at most one PTY surface and one Host Broker
capability registration. Activity monitors are bounded to the 32 most recent
sessions plus the selected session. Reconciliation is authoritative:
when a session disappears, the host cancels its monitor, removes its terminal
reference, clears unread state, and unregisters its preview credential. A
retained surface whose TUI process exited is recreated with a fresh credential.

Activity monitors attach through the same public protocol and classify only
semantic server events. Iteration and tool events are `RUNNING`; approvals,
questions, plans, and interaction timeouts are `NEEDS YOU`; terminal success
and failure events become `DONE` and `FAILED`. Changed state becomes unread only
for an unfocused session and clears when selected. Initial replay has a short
priming window so restored history does not masquerade as new attention.

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

Debug builds may use `SYLVANDER_TUI_PATH` as an explicit development or CI
override. Release builds ignore that environment variable and resolve only the
signed helper inside their own application bundle, so a caller cannot replace
the shipped TUI at launch time.

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

Sylvander uses those capabilities as a desktop host: native Swift manages
durable sessions and desktop-only inspectors, while every conversation remains
a real PTY surface running the packaged `sylvander-tui`.

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

### Sylvander integration location

Sylvander does not add a second Zig protocol stack or a non-PTY CoreSurface.
The integration is intentionally concentrated under
`macos/Sources/Features/Sylvander/`:

| Component | Responsibility |
|---|---|
| `SylvanderSessionClient` | Versioned Unix UI-protocol client for Agent/session discovery, lifecycle mutations, and activity streams |
| `SylvanderSessionStore` | Server-authoritative session list, selection persistence, reconnect, activity, and unread state |
| `SylvanderWorkspaceController` | One retained `Ghostty.SurfaceView` per server session; launch, focus, occlusion, retry, and reclamation |
| `SylvanderSessionSidebar` | Native left session rail and confirmed lifecycle actions |
| `SylvanderHostBroker` | Session-token-bound image/web preview requests from the embedded TUI |
| `SylvanderWorkspaceChrome` | Version-aware clear-glass/material fallback, context bar, and semantic host palette |
| `SylvanderChangesInspector` / `SylvanderPreviewInspector` | Desktop-only review surfaces outside the portable conversation UI |

`AppDelegate` owns one workspace controller. The controller creates a normal
Ghostty PTY surface whose command is the bundled `sylvander-tui`, whose working
directory is the session workspace, and whose environment carries the socket,
session, workspace, and optional Host Broker capability. Selecting a session
focuses its retained surface; removing the authoritative server session
destroys the reference and unregisters the host capability. The controller
also observes the selected PTY child: when the embedded TUI exits it retires
the dead surface on the next 0.5-second lifecycle poll, unregisters its Host
Broker capability, exposes a visible **Restart Terminal** action, and creates
a fresh bundled-helper surface without changing the server session.

This shape preserves both boundaries: the TUI remains the single conversation
implementation, and Ghostty remains the terminal renderer. No Swift message
view duplicates transcript, composer, approval, or tool rendering.

### Public UI protocol boundary

The desktop host speaks the current public UI protocol exactly; it does not
carry a pre-release compatibility range. `SylvanderSessionClient` sends
`min_version=5` and `max_version=5` on every connection and rejects any
negotiated version other than `5`.

| Desktop action | v5 client message | Expected v5 response |
|---|---|---|
| discover Agents | `discover_agents` | `agents_discovered` |
| list sessions | `list_sessions` | `sessions_list` |
| create a session | `create_session { request }` | `session_created` |
| rename/archive/delete | matching lifecycle message | `session_updated` / `session_deleted` |
| monitor activity | `load_session { session_id }` | session history followed by activity events |

Session creation uses the protocol-owned `SessionCreateRequest`. A desktop
workspace becomes `overrides.user_workspace` with
`execution_target="local"`, the selected path, and `read_only=false`. Swift
decoders intentionally project only fields needed by the desktop; additional
v5 Agent descriptor fields remain owned by the service.

## 7. transparency and terminal color contract

The workspace window is explicitly non-opaque with a clear AppKit background.
On macOS 26, `TerminalViewContainer` owns one clear
`NSGlassEffectView` derived from the packaged Ghostty configuration; the
Sylvander SwiftUI root stays `Color.clear` so it cannot stack a second dark
material beneath every terminal cell. On older macOS versions,
`SylvanderDesktopMaterial` remains the behind-window visual-effect fallback.
Panels and raised surfaces apply translucent semantic colors on top of either
host substrate. An opaque root fill is a regression on every version.

The workspace constructs `TerminalViewContainer` with
`dimsGlassWhenInactive=false`. Losing key-window focus must therefore preserve
the same clear-glass transparency instead of installing Ghostty's inactive
dark tint. Active/inactive changes may update focus and terminal behavior, but
must not turn the workspace opaque or visually replace the desktop material.

At application startup the host removes `NO_COLOR` from the process
environment; an empty value is not sufficient because many clients interpret
presence alone as a monochrome request. Each surface configuration then
passes:

- `SYLVANDER_TUI_COLOR=truecolor`;
- `CLICOLOR=1` and `CLICOLOR_FORCE=1`;
- `TERM=xterm-ghostty` and `COLORTERM=truecolor`;
- the session/socket/workspace variables.

Ghostty also publishes the same terminal capability while constructing the
child process. The explicit surface values make the Sylvander contract stable
if that inherited-environment path changes. The TUI still owns palette
selection and semantic color mapping; the host only guarantees that monochrome
state does not leak in from its parent process and that true-color capability
is visible.

`SylvanderSessionTests` verifies removal of `NO_COLOR`, non-opaque window
appearance, clear background, the older-system active material fallback, and
the exact surface color environment. Its macOS 26 clear-glass regression test
constructs the undimmed `TerminalViewContainer`, applies the packaged
clear-glass configuration, and requires a non-opaque host with an installed
glass effect. The packaged `Sylvander.ghostty` overlay fixes
`background-opacity=0.46`, applies it to explicitly painted cells, and selects
the clear macOS glass material. Terminal background cells therefore remain
partially transparent over the single host glass effect; the `0.36` canvas
token is reserved for native empty/inspector surfaces and is not an additional
full-window layer beneath a running TUI. TUI PTY tests cover the
`xterm-ghostty` terminal contract. Any change to launch environment or
workspace background must update both suites.

### Reproducible verification

Record bundle, lifecycle, and visual evidence in
[`ghostty-release-verification.md`](ghostty-release-verification.md); its
unchecked entries are release gates, not optional documentation.

From `sylvander-ghostty/macos`:

```sh
xcodebuild test \
  -project Sylvander.xcodeproj \
  -scheme Sylvander \
  -testPlan Sylvander \
  -destination 'platform=macOS' \
  -derivedDataPath /tmp/sylvander-ghostty-dd \
  -only-testing:GhosttyTests/SylvanderSessionTests
```

The suite exercises an actual `AF_UNIX` stream for exact-v5
hello/list/discover/create, store selection and lifecycle behavior, line
framing, translucent AppKit appearance, and the true-color launch environment.
After the build, inspect the actual packaged inputs rather than the source
copies:

```sh
cat /tmp/sylvander-ghostty-dd/Build/Products/Debug/Sylvander.app/Contents/Resources/Sylvander.ghostty
file /tmp/sylvander-ghostty-dd/Build/Products/Debug/Sylvander.app/Contents/Resources/bin/sylvander-tui
codesign --verify --strict \
  /tmp/sylvander-ghostty-dd/Build/Products/Debug/Sylvander.app/Contents/Resources/bin/sylvander-tui
```

The release-bundle contract is checked separately from the Debug test product:

```sh
cd sylvander-ghostty
nu macos/build.nu --configuration Release --action build

app=macos/build/Release/Sylvander.app
helper="$app/Contents/Resources/bin/sylvander-tui"
test -x "$helper"
lipo -archs "$helper"
codesign --verify --strict "$helper"
codesign --verify --deep --strict "$app"
```

The helper must contain both `arm64` and `x86_64`; Release launch resolution
must still ignore `SYLVANDER_TUI_PATH`. A local ad-hoc signature verifies
bundle integrity only. It is not evidence of Developer ID distribution
signing, notarization, or stapling; those remain credentialed release
prerequisites.

The ad-hoc Release product is intentionally not the local GUI smoke product:
without a Developer ID Team ID matching the embedded Sparkle framework,
hardened library validation rejects process launch. Run the same optimized
application locally with the checked-in `ReleaseLocal` configuration:

```sh
nu macos/build.nu --configuration ReleaseLocal --action build
open macos/build/ReleaseLocal/Sylvander.app
```

`ReleaseLocal` adds the explicit `disable-library-validation` development
entitlement. It is valid for the real Unix lifecycle, transparency, TrueColor,
focus, child-exit, and restart smoke checks, but it must never be described as
a production signature. Before distribution, repeat that lifecycle against a
consistently Developer ID-signed Release bundle and then run notarization,
stapling, and Gatekeeper assessment.

The focused suite covers the pure selected-child exit classifier, including
the invariant that a background or still-running surface is not retired.
Surface reuse, fresh `Ghostty.SurfaceView` construction, startup retry, and
session reclamation additionally require the real workspace smoke gate because
`Ghostty.SurfaceView` has no injectable factory. Any future factory seam must
stay under the Sylvander feature layer; it must not fork or mock Ghostty core.

## 8. cross-cutting concerns

### Service and capability boundaries

The native rail talks only to the public Unix UI protocol. It does not read the
Runtime database. Host previews use a separate process-private Unix socket and
256-bit session token; the Agent service never gains ambient desktop control.
The portable TUI remains usable without those optional capabilities.

### Threading and lifecycle

Ghostty's PTY and renderer retain their upstream thread model. Swift networking
and activity streams run asynchronously and publish observable state on the
main actor. Session refresh is bounded, reconnect uses backoff, activity
monitoring is capped, and hidden retained surfaces are marked occluded instead
of continuing foreground rendering.

### Logging and observability

The workspace controller uses the `ai.oraculo.sylvander` subsystem and logs
session identity plus lifecycle state, never prompt or credential content.
Host transport failures become visible connection or launch states. The TUI
continues to own turn/tool diagnostics and redacted exports.

### Fork maintenance

Sylvander-specific Swift files are additive. Upstream-facing edits are limited
to Xcode project/build hooks, bundle branding, and the AppDelegate entry point.
Before a Ghostty subtree sync, check `SYNCUP.md`; after it, rebuild the embedded
helper, run `SylvanderSessionTests`, and verify one real workspace window.

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
src/Surface.zig:5968-6012                  terminal desktop notifications
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
macos/Sources/App/macOS/AppDelegate.swift   owns the Sylvander workspace controller
macos/Sources/Features/Sylvander/
  SylvanderSessionClient.swift              public Unix UI-protocol adapter
  SylvanderSessionStore.swift               server-authoritative session state
  SylvanderWorkspaceController.swift        PTY surface lifecycle + TUI launch
  SylvanderSessionSidebar.swift             native left session rail
  SylvanderHostBroker.swift                 scoped desktop preview capability
  SylvanderWorkspaceChrome.swift            transparency + semantic host palette
macos/Scripts/embed-sylvander-tui.sh        packaged-helper validation and signing
macos/Tests/Sylvander/                      host lifecycle and appearance tests
```
