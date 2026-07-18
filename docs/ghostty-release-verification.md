# Ghostty desktop release verification

Status: local acceptance record  
Evidence date: 2026-07-18

This is the operator checklist for the macOS desktop host. It separates the
clean Release bundle gate, the locally launchable `ReleaseLocal` lifecycle
gate, and credentialed Apple distribution. A source assertion or Debug-only
result cannot be substituted for the corresponding application check, and a
`ReleaseLocal` launch is not a production-signing claim.

## Automated contract

- [x] The full Debug Swift test plan passes with exact UI protocol v5,
  real-`AF_UNIX` discovery/session fixtures, clear-window/material assertions,
  macOS 26 clear-glass construction, true-color launch environment, and
  selected-child exit classification.
- [x] The compiled Rust TUI PTY test passes under `TERM=xterm-ghostty`,
  `COLORTERM=truecolor`, `SYLVANDER_TUI_COLOR=truecolor`, and an explicitly
  absent `NO_COLOR`, and observes a 24-bit RGB SGR sequence.
- [x] A clean Release build contains an executable, signed
  `Contents/Resources/bin/sylvander-tui` with both `arm64` and `x86_64`.
- [x] A `ReleaseLocal` application connects to a real Sylvander Unix service,
  discovers/creates/selects a session, launches the bundled helper, survives
  reconnect, detects helper exit, and restarts a fresh terminal surface while
  retaining the server session.
- External deployment prerequisite: a Developer ID-signed Release application
  must pass library validation, launch, and complete the same real-service
  lifecycle before distribution. The local implementation gate cannot certify
  an Apple identity that is not present on the build host.

Reproduce the deterministic checks from the repository root:

```sh
cargo test -p sylvander-tui --test pty \
  binary_renders_across_compact_tmux_and_ghostty_term_surfaces --locked

(
  cd sylvander-ghostty
  nu macos/build.nu --configuration Debug --action test
  nu macos/build.nu --configuration Release --action build
  nu macos/build.nu --configuration ReleaseLocal --action build

  app=macos/build/Release/Sylvander.app
  helper="$app/Contents/Resources/bin/sylvander-tui"
  test -x "$helper"
  lipo -archs "$helper"
  codesign --verify --strict "$helper"
  codesign --verify --deep --strict "$app"
)
```

The helper architecture output must contain exactly `arm64` and `x86_64`
(order is not significant). Release launch resolution must ignore
`SYLVANDER_TUI_PATH` and use only the helper signed inside the bundle.

The clean ad-hoc Release bundle passed the helper architecture check and deep
strict code-signature verification on 2026-07-18. Its local launch is not a
distribution result: the ad-hoc application has no Developer ID Team ID that
matches the embedded Sparkle framework, so hardened library validation rejects
that process. Local lifecycle and visual acceptance therefore use the same
optimized `ReleaseLocal` build with its explicit
`disable-library-validation` development entitlement. Production Release
launch remains a deployment gate requiring one consistently Developer
ID-signed bundle.

The completed real-service `ReleaseLocal` acceptance run on 2026-07-18 used:

- application:
  `/Users/christmix/OraculoSpace/Sylvander/sylvander-ghostty/macos/build/ReleaseLocal/Sylvander.app`;
- Unix service: `/tmp/sylvander-ghostty-e2e/sylvander.sock` (initial server
  process `70320`, restarted process `8935`);
- application, initial bundled launcher, and initial actual TUI processes:
  `98753`, `98757`, and `98758`; restarted launcher and TUI processes: `7332`
  and `7333`; and
- server session `421360eb-14d8-43c4-9517-94317881b4d8`.

That optimized build succeeded; its main executable and helper both contain
`x86_64` and `arm64`, deep strict signature verification passed, and the
application carries the expected local-only library-validation entitlement.
The actual TUI process received `TERM=xterm-ghostty`,
`COLORTERM=truecolor`, `SYLVANDER_TUI_COLOR=truecolor`, and
`CLICOLOR_FORCE=1`. The parent launch environment deliberately contained
`NO_COLOR=1`, while the TUI environment did not, proving that the application
removes the inherited color suppression.

The same run completed the lifecycle boundary rather than inferring it from
unit tests:

1. The app discovered and selected the real persisted session, then launched
   the helper signed inside the application bundle.
2. Sending `SIGTERM` to actual TUI process `98758` retired both the launcher
   and child while application process `98753` remained alive. Within the
   0.5-second lifecycle polling interval the workspace displayed
   **Restart Terminal** and an explicit embedded-TUI-exited message.
3. Activating **Restart Terminal** produced launcher `7332` and actual TUI
   `7333`, retained session `421360eb-14d8-43c4-9517-94317881b4d8`, retained
   the socket/workspace values, and restored the same true-color environment
   without `NO_COLOR`.
4. Sending `SIGINT` to server `70320` removed the Unix socket and shut down
   its channel, agent, memory maintenance, and runtime cleanly. The application
   and restarted TUI processes remained alive. Starting server `8935` with the
   same data/config recreated the mode-`srw-------` socket; the two desktop/TUI
   clients authenticated again in about 0.4 seconds without replacing the app
   or TUI process or changing the session.

## Visual and interaction inspection

Capture one active-window and one inactive-window image from the same
`ReleaseLocal` build and real Unix-backed session. Repeat the lifecycle on the
Developer ID Release before distribution; the local captures do not certify
that signature.

- [x] Wallpaper or the window behind Sylvander remains perceptible through
  the terminal while foreground text remains readable.
- [x] Moving focus to another application does not install an opaque/dark
  inactive tint over the workspace.
- [x] Warm and violet identity colors plus status colors are visibly distinct;
  the TUI is not monochrome.
- [x] The native session rail remains left-aligned and the portable TUI remains
  a single-session conversation without a second sidebar.
- [x] After terminating the embedded TUI process, **Restart Terminal** is
  visible; activating it restores the TUI for the same server session.
- [x] No second Composer, duplicated transcript, or opaque full-window SwiftUI
  background appears during command, approval, or restart states.

The implementation contract behind this inspection is:

- non-opaque `NSWindow` with a clear background;
- one macOS 26 clear `TerminalViewContainer` glass effect, with a
  behind-window material fallback on older systems;
- `dimsGlassWhenInactive=false`;
- packaged `background-opacity=0.46`,
  `background-opacity-cells=true`, and
  `background-blur=macos-glass-clear`; and
- `NO_COLOR` removed before launch plus explicit Ghostty/true-color
  environment values on every surface.

The local visual evidence is:

- real-service initial workspace and true-color identity:
  `/var/folders/0p/65d_m6956tj7726tbvdgr2gh0000gn/T/com.openai.sky.CUAService/Sylvander Screenshot 2026-07-18 at 10.08.52 PM.jpeg`;
- helper-exit and restart affordance:
  `/var/folders/0p/65d_m6956tj7726tbvdgr2gh0000gn/T/com.openai.sky.CUAService/Sylvander Screenshot 2026-07-18 at 10.10.36 PM.jpeg`; and
- same-session terminal after restart:
  `/var/folders/0p/65d_m6956tj7726tbvdgr2gh0000gn/T/com.openai.sky.CUAService/Sylvander Screenshot 2026-07-18 at 10.11.48 PM.jpeg`;
- slash-command mode with one Composer, one command list, one status bar, and
  the welcome identity still present:
  `/var/folders/0p/65d_m6956tj7726tbvdgr2gh0000gn/T/com.openai.sky.CUAService/Sylvander Screenshot 2026-07-18 at 10.22.07 PM.jpeg`; and
- the non-key Sylvander window after raising Preview, retaining the same
  clear-glass base without the former dark opaque inactive tint:
  `/var/folders/0p/65d_m6956tj7726tbvdgr2gh0000gn/T/com.openai.sky.CUAService/Sylvander Screenshot 2026-07-18 at 10.22.50 PM.jpeg`.

A high-contrast Preview window was placed behind Sylvander for the transparency
inspection. Computer Use's window-only capture flattens the window alpha
against gray, so that capture is suitable evidence for layout and foreground
color, but it is not treated as composited transparency proof. Transparency
acceptance is the direct on-screen observation plus the non-opaque window,
clear-glass configuration, and focused construction tests above. The inactive
window capture records the separate focus-loss observation. Deleting the sole
`/` with Backspace also exited command mode immediately and restored the
single-line empty Composer.

## Deployment prerequisites

The local checks use an ad-hoc signature and a distinct `ReleaseLocal`
development entitlement. They do **not** prove Developer ID signing, hardened
library validation with consistently signed embedded frameworks, notarization,
stapling, Gatekeeper assessment, or update distribution. A distributable build
remains blocked until the deployment provides the Apple certificate, signing
identity, team/account credentials, and notary password required by the
release workflow.

Record the Release commit, Xcode/macOS version, `lipo` output, `codesign`
result, Unix socket used, and visual capture paths before changing the
remaining boxes to complete.
