//! Session — one AI agent conversation, owned by the server.
//!
//! A `Session` is the renderer-side mirror of a server-side agent
//! loop. The server owns the source of truth (message history,
//! working memory, background tasks). This struct holds the
//! materialised view plus enough transient state to drive the UI
//! (streaming buffer, approval prompts, etc.).
//!
//! Lifecycle:
//!
//! ```text
//! (none) ──list_sessions──► idle ──user message──► streaming
//!                              ▲                      │
//!                              │                      ▼
//!                            done ◄── agent_done ──  (server drives)
//!                              │
//!                              └─ background_task ─► (TUI closed, server keeps running)
//! ```
//!
//! Closing a Sylvander tab does **not** end the session on the server.
//! When the tab re-opens (or another tab subscribes), the server
//! replays any events that happened in the gap.

const std = @import("std");
const event = @import("event.zig");

/// One chat session as seen by the UI.
pub const Session = struct {
    allocator: std.mem.Allocator,

    /// Server-assigned identifier (uuid string).
    id: []u8,
    /// Human-readable name. Default is the first ~30 chars of the
    /// first user message.
    name: []u8,
    /// Workspace path (server-side). For local sessions this is the
    /// user machine; for cloud sessions it's an empty string.
    workspace: []u8,
    /// Model currently bound to this session (e.g. `claude-sonnet`).
    model: []u8,

    /// Ordered message history (user / agent / tool / system).
    messages: std.ArrayList(Message),
    /// Streaming buffer for the in-flight agent reply.
    streaming: std.ArrayList(u8),
    /// Streaming buffer for thinking content (if model emits it).
    streaming_thinking: std.ArrayList(u8),

    /// Session lifecycle phase.
    phase: Phase,
    /// Number of unread messages since the user last viewed this
    /// session (used by the sidebar to show "✓ N").
    unread: u32,
    /// Wall-clock timestamp of last activity, milliseconds since epoch.
    last_active_ms: i64,

    const Self = @This();

    pub const Message = union(enum) {
        user: []u8,
        agent: []u8,
        tool_call: ToolCall,
        tool_result: ToolResult,
        thinking: []u8,
        info: []u8,

        pub const ToolCall = struct {
            name: []u8,
            status: ToolStatus,
            input_raw: []u8,
        };

        pub const ToolStatus = enum { pending, done, errored };

        pub const ToolResult = struct {
            name: []u8,
            output: []u8,
            ok: bool,
        };
    };

    /// Session lifecycle phase. A tagged union so we can carry a
    /// payload (description / error message / etc.) per state.
    pub const Phase = union(enum) {
        /// Waiting for the user to type something.
        idle,
        /// Agent is streaming a reply right now.
        streaming,
        /// Server is waiting on the user to approve one or more tools.
        awaiting_approval,
        /// A long-running task is in flight; user has navigated away.
        background_task: BackgroundTask,
        /// Task finished while no UI was watching; sidebar badge "✓".
        completed_while_away: CompletedWhileAway,
        /// Last interaction errored out.
        errored: Errored,

        pub const BackgroundTask = struct {
            description: []u8,
        };

        pub const CompletedWhileAway = struct {
            finished_at_ms: i64,
            summary: []u8,
        };

        pub const Errored = struct {
            message: []u8,
        };
    };

    /// Free all owned memory. Safe to call multiple times.
    pub fn deinit(self: *Self) void {
        self.allocator.free(self.id);
        self.allocator.free(self.name);
        self.allocator.free(self.workspace);
        self.allocator.free(self.model);

        for (self.messages.items) |*msg| {
            switch (msg.*) {
                .user => |t| self.allocator.free(t),
                .agent => |t| self.allocator.free(t),
                .tool_call => |tc| {
                    self.allocator.free(tc.name);
                    self.allocator.free(tc.input_raw);
                },
                .tool_result => |tr| {
                    self.allocator.free(tr.name);
                    self.allocator.free(tr.output);
                },
                .thinking => |t| self.allocator.free(t),
                .info => |t| self.allocator.free(t),
            }
        }
        self.messages.deinit(self.allocator);

        self.streaming.deinit(self.allocator);
        self.streaming_thinking.deinit(self.allocator);

        switch (self.phase) {
            .background_task => |bt| self.allocator.free(bt.description),
            .completed_while_away => |c| self.allocator.free(c.summary),
            .errored => |er| self.allocator.free(er.message),
            else => {},
        }
    }

    /// Apply a domain event. Caller owns the event payload (we copy
    /// strings into `self`-owned buffers).
    pub fn apply(self: *Self, ev: event.DomainEvent) !void {
        switch (ev) {
            .connected, .tick => {},
            .disconnected => |d| {
                try self.setPhase(.{ .errored = .{
                    .message = try self.allocator.dupe(u8, d.reason),
                } });
            },
            .session_created => |s| {
                self.id = try self.allocator.dupe(u8, s.session_id);
            },
            .text_chunk => |c| try self.streaming.appendSlice(self.allocator, c.delta),
            .thinking_chunk => |c| try self.streaming_thinking.appendSlice(self.allocator, c.delta),
            .tool_started => |t| {
                const name = try self.allocator.dupe(u8, t.tool_name);
                try self.messages.append(self.allocator, .{
                    .tool_call = .{
                        .name = name,
                        .status = .pending,
                        .input_raw = try self.allocator.dupe(u8, ""),
                    },
                });
                try self.setPhase(.streaming);
            },
            .tool_finished => |t| {
                // Find the matching pending ToolCall and flip its status.
                var i: usize = self.messages.items.len;
                while (i > 0) {
                    i -= 1;
                    if (self.messages.items[i] == .tool_call) {
                        var tc = &self.messages.items[i].tool_call;
                        if (std.mem.eql(u8, tc.name, t.tool_name) and tc.status == .pending) {
                            tc.status = if (t.is_error) .errored else .done;
                            break;
                        }
                    }
                }
                const name = try self.allocator.dupe(u8, t.tool_name);
                const output = try self.allocator.dupe(u8, t.output);
                try self.messages.append(self.allocator, .{
                    .tool_result = .{
                        .name = name,
                        .output = output,
                        .ok = !t.is_error,
                    },
                });
            },
            .agent_done => |d| {
                if (self.streaming.items.len > 0) {
                    const text = try self.streaming.toOwnedSlice(self.allocator);
                    try self.messages.append(self.allocator, .{ .agent = text });
                } else if (d.final_text.len > 0) {
                    const text = try self.allocator.dupe(u8, d.final_text);
                    try self.messages.append(self.allocator, .{ .agent = text });
                }
                self.streaming_thinking.clearRetainingCapacity();
                try self.setPhase(.idle);
            },
            .agent_error_event => |e| {
                try self.setPhase(.{ .errored = .{
                    .message = try self.allocator.dupe(u8, e.message),
                } });
                self.streaming.clearRetainingCapacity();
                self.streaming_thinking.clearRetainingCapacity();
            },
            .approval_requested => {
                try self.setPhase(.awaiting_approval);
            },
        }
        self.last_active_ms = std.time.milliTimestamp();
    }

    fn setPhase(self: *Self, new: Phase) !void {
        // Free old payload if it owned one.
        switch (self.phase) {
            .background_task => |bt| self.allocator.free(bt.description),
            .completed_while_away => |c| self.allocator.free(c.summary),
            .errored => |er| self.allocator.free(er.message),
            else => {},
        }
        self.phase = new;
    }
};