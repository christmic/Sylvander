//! Configuration types — wired into Ghostty's `Config.zig` in F2.
//!
//! Today these are standalone structs that can be hand-constructed in
//! tests. In F2 we'll plug them into Ghostty's config schema so that
//!
//! ```toml
//! [sylvander.connection]
//! prefer = "prefer_local"
//! local_path = "/tmp/sylvander.sock"
//! remote = "wss://sylvander.example.com"
//!
//! [sylvander.workbench]
//! sidebar_width = 32
//! default_model = "claude-sonnet"
//!
//! [sylvander.notify]
//! desktop = true
//! ```
//!
//! parses into the structs below.

const std = @import("std");

pub const Connection = struct {
    /// Local-first: try Unix socket, fall back to WSS.
    /// Local-only: refuse to start if the socket is missing.
    /// Remote-only: go straight to WSS.
    prefer: Prefer = .prefer_local,

    /// Filesystem path for the local Unix socket.
    /// Default: `/tmp/sylvander.sock`.
    local_path: []const u8 = "/tmp/sylvander.sock",

    /// WSS endpoint for cross-device usage.
    remote: []const u8 = "",

    pub const Prefer = enum {
        prefer_local,
        local_only,
        remote_only,
    };
};

pub const Workbench = struct {
    /// Width of the left session list, in columns.
    sidebar_width: u16 = 32,

    /// Model identifier to default new sessions to.
    default_model: []const u8 = "claude-sonnet",

    /// Path on disk where TUI history is cached (optional).
    /// Server-side history is authoritative; this is purely a
    /// render hint for offline display.
    cache_path: ?[]const u8 = null,
};

pub const Notify = struct {
    /// Use the host OS notification API (macOS NSUserNotification,
    /// Linux libnotify, Windows Toast) when an agent finishes a
    /// turn while the tab is not focused.
    desktop: bool = true,

    /// Play a short sound on completion. Off by default to avoid
    /// being annoying.
    sound: bool = false,

    /// Minimum task duration before we consider it worth a
    /// notification. Below this, stay silent.
    min_duration_ms: u32 = 2_000,
};

pub const Config = struct {
    enabled: bool = false,
    connection: Connection = .{},
    workbench: Workbench = .{},
    notify: Notify = .{},
};