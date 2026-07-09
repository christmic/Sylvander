//! Sylvander — first-class AI agent tab inside Ghostty.
//!
//! This module transforms Ghostty from a generic terminal emulator into
//! the native frontend for the Sylvander AI agent framework. It adds:
//!
//! - **Sylvander tab**: a new tab kind that, instead of running a PTY,
//!   renders a workbench UI (sidebar + tabs + chat + input + status bar)
//!   driven by a Sylvander server connection.
//! - **Multi-session**: each Sylvander tab holds one session; the host
//!   app can manage many of them, mirroring an IDE-like workflow.
//! - **Native notifications**: agent events route through Ghostty's
//!   desktop notification API instead of shelling out to `notify-send`.
//! - **Persistent state**: sessions live on the server; closing the
//!   tab does not interrupt background tasks.
//!
//! # Architecture
//!
//! ```
//! ┌─────────────────────────────────────────────────────────────┐
//! │  Sylvander Tab (this module)                                 │
//! │  ┌──────────────────────────────────────────────────────┐    │
//! │  │ connection.zig  ─►  server (WSS / Unix socket)        │    │
//! │  │ session.zig     ─►  per-session state machine         │    │
//! │  │ event.zig       ─►  wire-format types (JSON)          │    │
//! │  │ renderer.zig    ─►  workbench layout (sidebar + chat) │    │
//! │  │ config.zig      ─►  user-facing settings              │    │
//! │  └──────────────────────────────────────────────────────┘    │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Wire protocol
//!
//! Sylvander talks the same JSON-line protocol as
//! `sylvander-channel-unix` / `sylvander-channel-ws` on the server
//! side. One JSON object per line; tagged-union encoding via the
//! `"type"` field. See `event.zig` for the canonical types and
//! `protocol.zig` for the framing rules.
//!
//! # Sync strategy
//!
//! `sylvander-ghostty/` is a `git subtree` mirror of Ghostty upstream.
//! See SYNCUP.md for the routine. To minimise merge friction we:
//!
//! - Add files in `src/sylvander/`; rarely edit upstream files.
//! - When we must patch upstream code, gate the change behind
//!   `if (config.sylvander.enabled)` so the patch is a no-op for
//!   non-Sylvander builds.
//! - Expose everything through `apprt.zig` (the runtime abstraction)
//!   so macOS / Linux / GTK ports pick it up uniformly.
//!
//! # Status
//!
//! | Phase | Scope                              | State      |
//! |-------|------------------------------------|------------|
//! | F1    | Subtree import + module skeleton   | in progress |
//! | F2    | SylvanderTab type (no I/O)         | pending     |
//! | F3    | Connection + session management    | pending     |
//! | F4    | Workbench renderer + interaction   | pending     |
//! | F5    | Status bar + native notifications  | pending     |
//! | F6    | Persistence integration            | pending     |

const std = @import("std");

pub const Connection = @import("connection.zig");
pub const Session = @import("session.zig");
pub const Event = @import("event.zig");
pub const Config = @import("config.zig");
pub const Protocol = @import("protocol.zig");

/// Bumped by hand whenever `event.zig` or `protocol.zig` changes in a
/// way that is not backward-compatible with previous server builds.
pub const protocol_version: u32 = 1;

test "module wiring" {
    // Smoke test: importing every submodule compiles.
    _ = Connection;
    _ = Session;
    _ = Event;
    _ = Config;
    _ = Protocol;
}