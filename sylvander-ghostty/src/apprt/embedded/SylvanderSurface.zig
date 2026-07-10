//! SylvanderSurface.zig
//!
//! F2 skeleton: a placeholder module that compiles but does not yet bind to
//! the Swift SylvanderController. The actual UI lives in
//! `macos/Sources/Features/Sylvander/` and renders SwiftUI components
//! driven by a mock client (see `SylvanderClient.swift`).
//!
//! F3 will:
//!   1. Add a `WebSocketClient` in `src/sylvander_net/client.zig` using
//!      `std.http.Client` + a hand-rolled ws frame parser.
//!   2. Wire this module to bridge `SylvanderSurface.zig` ⇄ Swift via the
//!      existing `ghostty_runtime_config_s` callback table (no new C ABI).
//!   3. Replace the Swift mock client with the real WSS stream.
//!
//! Keeping this file as a stub until F3 avoids touching the C ABI now,
//! which would force a full GhosttyKit.xcframework rebuild and slow the
//! F2 visual iteration loop.

const std = @import("std");

pub const SylvanderConfig = struct {
    /// WebSocket URL of the Sylvander server. F2: ignored.
    server_url: []const u8 = "ws://127.0.0.1:9527/ws",
    /// Auth token (bearer). F2: ignored.
    auth_token: []const u8 = "",
};

/// F2 stub: holds the parsed config so we can verify the file is wired into
/// the build. F3 will replace this with a real WSS session handle.
pub const SylvanderSurface = struct {
    config: SylvanderConfig,
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator, config: SylvanderConfig) SylvanderSurface {
        return .{ .allocator = allocator, .config = config };
    }

    pub fn deinit(self: *SylvanderSurface) void {
        _ = self;
    }
};

test "sylvander surface config has sane defaults" {
    const cfg = SylvanderConfig{};
    try std.testing.expect(cfg.server_url.len > 0);
    try std.testing.expectEqualStrings("", cfg.auth_token);
}

test "sylvander surface init/deinit is a no-op" {
    const allocator = std.testing.allocator;
    var surface = SylvanderSurface.init(allocator, .{});
    defer surface.deinit();
}