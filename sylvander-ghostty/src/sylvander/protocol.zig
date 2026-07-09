//! Wire-protocol framing rules shared by the connection layer.
//!
//! Sylvander speaks a **line-delimited JSON** protocol on top of
//! either WebSocket (cross-device) or Unix domain socket (same-host).
//! One JSON object per line; lines are separated by a single `\n` byte.
//! No framing beyond that — no length prefix, no chunked encoding.
//!
//! Encoding style (matched to `sylvander-channel-unix` on the server):
//!
//! - All messages are JSON objects with a `type` field.
//! - Variants are tagged unions via `#[serde(tag = "type")]` on the
//!   Rust side; we mirror this by always emitting a `"type"` string.
//! - Field names are `snake_case`.
//!
//! Why line-delimited JSON instead of something faster:
//!
//! - Trivial to debug (`cat /tmp/sylvander.sock | jq`).
//! - The server's existing channel implementations already use it.
//! - Parsing is fast enough at human-message cadence; for bulk
//!   transport we'd revisit.
//!
//! See `event.zig` for the message types.

const std = @import("std");

/// Maximum size of a single JSON line. Anything beyond this is a
/// protocol violation and we close the connection.
pub const max_line_bytes: usize = 1024 * 1024; // 1 MiB

/// Line terminator. Always LF; CRLF is normalized on read.
pub const newline: u8 = '\n';

/// Errors that the connection layer may surface up.
pub const ProtocolError = error{
    LineTooLong,
    InvalidJson,
    UnexpectedEof,
    UnsupportedMessageType,
    VersionMismatch,
};

/// Result of a single `next()` call on the read half of a connection.
pub const ReadResult = union(enum) {
    /// A complete JSON line was parsed.
    line: []const u8,
    /// The remote closed cleanly.
    eof,
};