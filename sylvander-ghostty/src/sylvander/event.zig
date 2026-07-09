//! Domain events and wire-format types.
//!
//! This file mirrors the Rust types in `sylvander-protocol`:
//! - `StreamEvent` — transient events emitted during agent loop execution
//! - `DomainEvent` — what the UI Reducer consumes
//! - `ClientMsg` — what we send to the server
//! - `ServerMsg` — what the server sends to us
//!
//! All variants are tagged unions keyed on the JSON `"type"` field.
//! See `protocol.zig` for the framing rules.
//!
//! The shapes here are intentionally narrow: only what the renderer
//! and the connection layer need. Server-side-only fields (e.g.
//! internal agent IDs) are intentionally omitted.

const std = @import("std");

// ===========================================================================
// Server → Client
// ===========================================================================

pub const ServerMsg = union(enum) {
    session_created: SessionCreated,
    text_delta: TextDelta,
    thinking_delta: ThinkingDelta,
    tool_call: ToolCallEvent,
    tool_result: ToolResultEvent,
    iteration_start: IterationStart,
    done: Done,
    err: ErrorMsg,
    approval_request: ApprovalRequest,
    pong: Pong,

    pub const SessionCreated = struct {
        session_id: []const u8,
    };

    pub const TextDelta = struct {
        session_id: []const u8,
        delta: []const u8,
    };

    pub const ThinkingDelta = struct {
        session_id: []const u8,
        delta: []const u8,
    };

    pub const ToolCallEvent = struct {
        session_id: []const u8,
        tool_name: []const u8,
    };

    pub const ToolResultEvent = struct {
        session_id: []const u8,
        tool_name: []const u8,
        output: []const u8,
        is_error: bool,
    };

    pub const IterationStart = struct {
        session_id: []const u8,
        iteration: u32,
    };

    pub const Done = struct {
        session_id: []const u8,
        text: []const u8,
    };

    pub const ErrorMsg = struct {
        session_id: []const u8,
        message: []const u8,
    };

    pub const ApprovalRequest = struct {
        session_id: []const u8,
        batch_id: []const u8,
        tools: []const ToolInfo,
    };

    pub const ToolInfo = struct {
        call_id: []const u8,
        tool_name: []const u8,
        // `input` is left as a JSON value blob — we don't interpret it.
        input_raw: []const u8,
    };

    pub const Pong = struct {};
};

// ===========================================================================
// Client → Server
// ===========================================================================

pub const ClientMsg = union(enum) {
    chat: Chat,
    approve: Approve,
    answer: Answer,
    ping: Ping,

    pub const Chat = struct {
        text: []const u8,
        session_id: ?[]const u8 = null,
    };

    pub const Approve = struct {
        call_id: []const u8,
        approved: bool,
    };

    pub const Answer = struct {
        call_id: []const u8,
        answer: []const u8,
    };

    pub const Ping = struct {};
};

// ===========================================================================
// Internal domain events (after protocol → domain translation)
// ===========================================================================

/// Events consumed by the UI reducer. Translated from `ServerMsg`
/// inside `connection.zig` so the rest of the renderer never sees
/// the wire format.
pub const DomainEvent = union(enum) {
    connected,
    disconnected: Disconnected,
    session_created: SessionCreatedRef,
    text_chunk: TextChunk,
    thinking_chunk: ThinkingChunk,
    tool_started: ToolStarted,
    tool_finished: ToolFinished,
    agent_done: AgentDone,
    agent_error_event: AgentError,
    approval_requested: ApprovalRequested,
    tick,

    pub const Disconnected = struct {
        reason: []const u8,
    };

    pub const SessionCreatedRef = struct {
        session_id: []const u8,
    };

    pub const TextChunk = struct {
        delta: []const u8,
    };

    pub const ThinkingChunk = struct {
        delta: []const u8,
    };

    pub const ToolStarted = struct {
        tool_name: []const u8,
    };

    pub const ToolFinished = struct {
        tool_name: []const u8,
        output: []const u8,
        is_error: bool,
    };

    pub const AgentDone = struct {
        final_text: []const u8,
    };

    pub const AgentError = struct {
        message: []const u8,
    };

    pub const ApprovalRequested = struct {
        batch_id: []const u8,
        tools: []const ToolSummary,
    };

    pub const ToolSummary = struct {
        call_id: []const u8,
        tool_name: []const u8,
    };
};

/// Outbound side effects the reducer hands back to the connection
/// layer. `Quit` is consumed by the host (e.g. the App loop) and not
/// sent over the wire.
pub const Action = union(enum) {
    send_chat: SendChat,
    send_approve: SendApprove,
    send_answer: SendAnswer,
    quit,

    pub const SendChat = struct {
        text: []const u8,
        session_id: ?[]const u8,
    };

    pub const SendApprove = struct {
        call_id: []const u8,
        approved: bool,
    };

    pub const SendAnswer = struct {
        call_id: []const u8,
        answer: []const u8,
    };
};