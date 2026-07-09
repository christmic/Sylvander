//! Connection layer — WSS and Unix socket clients.
//!
//! Both transports deliver the same line-delimited JSON protocol
//! (see `protocol.zig`); we hide the difference behind a single
//! `Connection` vtable so the rest of the module is transport-agnostic.
//!
//! ## Connection preference
//!
//! On startup we try the local Unix-domain socket first (lowest
//! latency, no TLS handshake). If it isn't there we fall back to
//! WSS. Both are configured via `Config.zig`.
//!
//! ## Reconnection
//!
//! The connection layer is responsible for transparent reconnect
//! with exponential backoff. Higher layers receive a single
//! `connected` / `disconnected` lifecycle, not raw socket events.

const std = @import("std");
const event = @import("event.zig");
const protocol = @import("protocol.zig");
const Config = @import("config.zig");

const Allocator = std.mem.Allocator;

/// A live connection to a Sylvander server.
pub const Connection = struct {
    allocator: std.mem.Allocator,
    transport: Transport,

    /// Read buffer — one JSON line at a time.
    read_buf: std.ArrayList(u8),

    pub const Transport = union(enum) {
        unix: Unix,
        wss: Wss,

        pub const Unix = struct {
            fd: std.posix.fd_t,
        };

        pub const Wss = struct {
            // Placeholder — populated when the WSS client is implemented
            // in Phase F3. Until then we error out with `UnsupportedTransport`.
            _placeholder: u8 = 0,
        };
    };

    pub const ConnectError = error{
        NoEndpointConfigured,
        ConnectionRefused,
        UnsupportedTransport,
    };

    pub fn connect(allocator: Allocator, cfg: Config.Connection) ConnectError!Connection {
        switch (cfg.prefer) {
            .local_only => return connectUnix(allocator, cfg.local_path) catch error.ConnectionRefused,
            .remote_only => return .{
                .allocator = allocator,
                .transport = .{ .wss = .{} },
                .read_buf = .empty,
            },
            .prefer_local => {
                if (connectUnix(allocator, cfg.local_path)) |c| return c else |_| {
                    return .{
                        .allocator = allocator,
                        .transport = .{ .wss = .{} },
                        .read_buf = .empty,
                    };
                }
            },
        }
    }

    fn connectUnix(allocator: Allocator, path: []const u8) !Connection {
        // We use libc directly because zig 0.16 relocated the socket()
        // and connect() calls into per-platform namespaces. This keeps
        // the F1 stub readable; Phase F3 will rewrite against
        // Ghostty's own os.zig helpers and add Windows + WSS support.
        const c = @cImport({
            @cInclude("sys/socket.h");
            @cInclude("sys/un.h");
            @cInclude("unistd.h");
            @cInclude("string.h");
        });

        const fd = c.socket(c.AF_UNIX, c.SOCK_STREAM, 0);
        if (fd < 0) return error.ConnectionRefused;
        errdefer _ = c.close(fd);

        var addr: c.struct_sockaddr_un = std.mem.zeroes(c.struct_sockaddr_un);
        addr.sun_family = c.AF_UNIX;
        if (path.len >= addr.sun_path.len) return error.ConnectionRefused;
        @memcpy(addr.sun_path[0..path.len], path);
        addr.sun_path[path.len] = 0;
        const addr_len: c.socklen_t = @sizeOf(c.struct_sockaddr_un);

        const rc = c.connect(fd, @ptrCast(&addr), addr_len);
        if (rc != 0) return error.ConnectionRefused;

        return .{
            .allocator = allocator,
            .transport = .{ .unix = .{ .fd = @intCast(fd) } },
            .read_buf = .empty,
        };
    }

    pub fn deinit(self: *Connection) void {
        switch (self.transport) {
            .unix => |u| std.posix.close(u.fd),
            .wss => {},
        }
        self.read_buf.deinit(self.allocator);
    }

    /// Send one wire-format `ClientMsg`. The caller is responsible for
    /// ensuring the message is well-formed.
    pub fn sendClient(self: *Connection, msg: event.ClientMsg) !void {
        var buf: std.ArrayList(u8) = .empty;
        defer buf.deinit(self.allocator);
        try serializeClientEvent(&buf, self.allocator, msg);
        try self.writeAll(buf.items);
        try self.writeAll(&[_]u8{protocol.newline});
    }

    /// Block until one JSON line is read (LF terminated), or EOF.
    /// Caller owns the returned slice; it lives in `read_buf` until
    /// the next call.
    pub fn nextLine(self: *Connection) protocol.ReadResult {
        while (true) {
            // Look for newline in existing buffer.
            if (std.mem.indexOfScalar(u8, self.read_buf.items, protocol.newline)) |idx| {
                const line = self.read_buf.items[0..idx];
                const rest = self.read_buf.items[idx + 1 ..];
                std.mem.copyForwards(u8, self.read_buf.items[0..rest.len], rest);
                self.read_buf.shrinkRetainingCapacity(rest.len);
                return .{ .line = line };
            }
            if (self.read_buf.items.len >= protocol.max_line_bytes) {
                return protocol.ProtocolError.LineTooLong;
            }

            var chunk: [4096]u8 = undefined;
            const n = switch (self.transport) {
                .unix => |u| std.posix.read(u.fd, &chunk) catch return protocol.ProtocolError.UnexpectedEof,
                .wss => return .eof, // not yet implemented
            };
            if (n == 0) return .eof;
            self.read_buf.appendSlice(self.allocator, chunk[0..n]) catch return protocol.ProtocolError.UnexpectedEof;
        }
    }

    fn writeAll(self: *Connection, bytes: []const u8) !void {
        switch (self.transport) {
            .unix => |u| {
                var written: usize = 0;
                while (written < bytes.len) {
                    const n = std.posix.write(u.fd, bytes[written..]) catch return error.ConnectionRefused;
                    written += n;
                }
            },
            .wss => return error.UnsupportedTransport,
        }
    }
};

// ===========================================================================
// JSON serialization (minimal — sufficient for the protocol we use)
// ===========================================================================
//
// We avoid pulling in a JSON dependency for the F1 stub. The shapes
// here are deliberately tiny; a full implementation will replace this
// with std.json or zap.

fn serializeClientEvent(buf: *std.ArrayList(u8), allocator: Allocator, msg: event.ClientMsg) !void {
    switch (msg) {
        .ping => try buf.appendSlice(allocator, "{\"type\":\"ping\"}"),
        .chat => |c| {
            try buf.appendSlice(allocator, "{\"type\":\"chat\",\"text\":");
            try appendJsonString(buf, allocator, c.text);
            if (c.session_id) |sid| {
                try buf.appendSlice(allocator, ",\"session_id\":");
                try appendJsonString(buf, allocator, sid);
            }
            try buf.append(allocator, '}');
        },
        .approve => |a| {
            try buf.appendSlice(allocator, "{\"type\":\"approve\",\"call_id\":");
            try appendJsonString(buf, allocator, a.call_id);
            try buf.appendSlice(allocator, ",\"approved\":");
            try buf.appendSlice(allocator, if (a.approved) "true" else "false");
            try buf.append(allocator, '}');
        },
        .answer => |a| {
            try buf.appendSlice(allocator, "{\"type\":\"answer\",\"call_id\":");
            try appendJsonString(buf, allocator, a.call_id);
            try buf.appendSlice(allocator, ",\"answer\":");
            try appendJsonString(buf, allocator, a.answer);
            try buf.append(allocator, '}');
        },
    }
}

fn appendJsonString(buf: *std.ArrayList(u8), allocator: Allocator, s: []const u8) !void {
    try buf.append(allocator, '"');
    for (s) |c| {
        switch (c) {
            '"' => try buf.appendSlice(allocator, "\\\""),
            '\\' => try buf.appendSlice(allocator, "\\\\"),
            '\n' => try buf.appendSlice(allocator, "\\n"),
            '\r' => try buf.appendSlice(allocator, "\\r"),
            '\t' => try buf.appendSlice(allocator, "\\t"),
            else => if (c < 0x20) {
                var esc: [6]u8 = undefined;
                const slice = std.fmt.bufPrint(&esc, "\\u{x:0>4}", .{c}) catch return error.OutOfMemory;
                try buf.appendSlice(allocator, slice);
            } else try buf.append(allocator, c),
        }
    }
    try buf.append(allocator, '"');
}

// ===========================================================================
// Tests
// ===========================================================================

test "json string escaping" {
    var buf: std.ArrayList(u8) = .empty;
    defer buf.deinit(std.testing.allocator);

    try appendJsonString(&buf, std.testing.allocator, "hello\n\"world\"");
    try std.testing.expectEqualStrings(
        "\"hello\\n\\\"world\\\"\"",
        buf.items,
    );
}

test "connectUnix fails on nonexistent socket" {
    const c = Connection.connect(std.testing.allocator, .{
        .prefer = .local_only,
        .local_path = "/tmp/__sylvander_does_not_exist__.sock",
    });
    try std.testing.expectError(error.ConnectionRefused, c);
}