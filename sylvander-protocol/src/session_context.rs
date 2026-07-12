//! `SessionContext` — the "who, where, when, why" of a request.
//!
//! This struct is the single source of truth for everything that
//! describes *which* session/user/agent is asking *what* and *from
//! where*. It's the answer to "I have a piece of work; tell me
//! everything I need to know to do it right and to do it safely".
//!
//! # Why one struct, not scattered fields?
//!
//! Earlier designs had `user_id`, `agent_id`, `session_id`, `workspace`,
//! `channel`, `priority`, `trace_id` ... sprinkled across every API
//! signature. That breaks the moment you add `tenant_id` or
//! `experiment_bucket` — every caller has to change.
//!
//! `SessionContext` packs all of that into one place, with a
//! `AttributeBag` for things we haven't thought of yet. New fields
//! only touch the struct, never the call sites.
//!
//! # Distinction from `ToolContext`
//!
//! `ToolContext` (in `sylvander-agent`) is the *per-invocation* input
//! to a tool — it owns a `SessionContext` for identity but also carries
//! tool-specific concerns like execution budget and surface
//! capabilities. The two should not be confused: `SessionContext` is
//! "who is asking", `ToolContext` is "everything the tool needs to run".

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::{AgentId, SessionId, UserId};

// ===========================================================================
// Identity — "who" is asking
// ===========================================================================

/// Identifies the user, agent, and session of a request.
///
/// One user owns many agents; one agent participates in many sessions;
/// one session is bound to exactly one user and one agent (for the
/// request lifetime).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub user_id: UserId,
    pub agent_id: AgentId,
    pub session_id: SessionId,
}

impl Identity {
    /// Sentinel for system-originated actions (cron, internal tasks)
    /// that have no real user / agent / session. Three distinct
    /// sentinels so cross-checks catch accidental mixing.
    #[must_use]
    pub fn system() -> Self {
        Self {
            user_id: UserId::system(),
            agent_id: AgentId::new("__system_agent__"),
            session_id: SessionId::new("__system_session__"),
        }
    }
}

// ===========================================================================
// Origin — "where" the request is coming from
// ===========================================================================

/// Where the request originated: which workspace, which channel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// Working directory for file operations. May be unset for
    /// system-originated requests.
    pub workspace: Option<PathBuf>,
    /// Channel name (telegram, wechat, tui, ws, ...). Free-form string
    /// so the protocol doesn't need an enum per channel type.
    pub channel: Option<String>,
}

// ===========================================================================
// RequestMeta — "when" and "why"
// ===========================================================================

/// Per-request metadata: timing, tracing, priority.
///
/// Mostly for observability and routing. Tools and stores should
/// generally not branch on these — read `Identity` for
/// access-control decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestMeta {
    /// Unix epoch seconds when the request was created.
    pub created_at: i64,
    /// Optional correlation id propagated through the call graph.
    pub trace_id: Option<String>,
    /// Scheduling / queue priority. Default = Normal.
    pub priority: Priority,
}

impl Default for RequestMeta {
    fn default() -> Self {
        Self {
            created_at: 0,
            trace_id: None,
            priority: Priority::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Urgent,
}

// ===========================================================================
// AttributeBag — free-form, future-proof
// ===========================================================================

/// Free-form key/value bag for fields that don't yet have a slot in
/// the typed fields above.
///
/// New experimental or cross-cutting fields land here first; once
/// they prove stable they migrate to typed fields. This way adding
/// a new field never breaks call sites.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributeBag {
    inner: HashMap<String, AttributeValue>,
}

impl AttributeBag {
    /// Empty bag.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style insert. Returns self for chaining.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<AttributeValue>) -> Self {
        self.inner.insert(key.into(), value.into());
        self
    }

    /// Insert / replace a value. Returns `Some(previous)` if the key
    /// already existed.
    pub fn set(
        &mut self,
        key: impl Into<String>,
        value: impl Into<AttributeValue>,
    ) -> Option<AttributeValue> {
        self.inner.insert(key.into(), value.into())
    }

    /// Get a value by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&AttributeValue> {
        self.inner.get(key)
    }

    /// Get a string value by key (returns `None` if not present or
    /// not a string).
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(AttributeValue::as_str)
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` if no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Iterate over all (key, value) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &AttributeValue)> {
        self.inner.iter().map(|(k, v)| (k.as_str(), v))
    }
}

/// Typed value for an attribute entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttributeValue {
    String(String),
    Int(i64),
    Bool(bool),
}

impl AttributeValue {
    /// If the value is a string, return a borrow.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// If the value is an int, return it.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// If the value is a bool, return it.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

impl From<&str> for AttributeValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}
impl From<String> for AttributeValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}
impl From<i64> for AttributeValue {
    fn from(n: i64) -> Self {
        Self::Int(n)
    }
}
impl From<bool> for AttributeValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

// ===========================================================================
// SessionContext — the umbrella
// ===========================================================================

/// The full context for a session-scoped operation.
///
/// Every API that needs to know "who, where, when, why" takes this
/// as its first argument. To add a new field, extend the relevant
/// sub-struct — call sites don't change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionContext {
    pub identity: Identity,
    pub origin: Origin,
    pub request: RequestMeta,
    pub attributes: AttributeBag,
}

impl SessionContext {
    /// Minimal constructor: identity only. Origin/RequestMeta get
    /// defaults, attributes is empty.
    #[must_use]
    pub fn new(
        user_id: impl Into<UserId>,
        agent_id: impl Into<AgentId>,
        session_id: impl Into<SessionId>,
    ) -> Self {
        Self {
            identity: Identity {
                user_id: user_id.into(),
                agent_id: agent_id.into(),
                session_id: session_id.into(),
            },
            origin: Origin::default(),
            request: RequestMeta {
                created_at: crate::types::now_secs(),
                trace_id: None,
                priority: Priority::default(),
            },
            attributes: AttributeBag::default(),
        }
    }

    /// Sentinel for system-originated requests.
    #[must_use]
    pub fn system() -> Self {
        Self {
            identity: Identity::system(),
            origin: Origin::default(),
            request: RequestMeta {
                created_at: crate::types::now_secs(),
                trace_id: None,
                priority: Priority::Normal,
            },
            attributes: AttributeBag::default(),
        }
    }

    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.origin.workspace = Some(workspace.into());
        self
    }

    #[must_use]
    pub fn with_channel(mut self, channel: impl Into<String>) -> Self {
        self.origin.channel = Some(channel.into());
        self
    }

    #[must_use]
    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.request.trace_id = Some(trace_id.into());
        self
    }

    #[must_use]
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.request.priority = priority;
        self
    }

    #[must_use]
    pub fn with_attribute(
        mut self,
        key: impl Into<String>,
        value: impl Into<AttributeValue>,
    ) -> Self {
        self.attributes.set(key, value);
        self
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_identity_with_defaults() {
        let ctx = SessionContext::new("alice", "code-assistant", "sess-1");
        assert_eq!(ctx.identity.user_id.0, "alice");
        assert_eq!(ctx.identity.agent_id.0, "code-assistant");
        assert_eq!(ctx.identity.session_id.0, "sess-1");
        assert!(ctx.origin.workspace.is_none());
        assert!(ctx.origin.channel.is_none());
        assert_eq!(ctx.request.priority, Priority::Normal);
        assert!(ctx.attributes.is_empty());
    }

    #[test]
    fn system_sentinel_is_distinct() {
        let sys = SessionContext::system();
        let real = SessionContext::new("alice", "a", "s");
        assert_ne!(sys.identity, real.identity);
    }

    #[test]
    fn builder_chain_sets_optional_fields() {
        let ctx = SessionContext::new("alice", "a", "s")
            .with_workspace("/home/alice/code")
            .with_channel("telegram")
            .with_trace_id("req-42")
            .with_priority(Priority::High)
            .with_attribute("experiment", "control")
            .with_attribute("attempt", 3_i64);

        assert_eq!(
            ctx.origin.workspace.as_deref(),
            Some(std::path::Path::new("/home/alice/code"))
        );
        assert_eq!(ctx.origin.channel.as_deref(), Some("telegram"));
        assert_eq!(ctx.request.trace_id.as_deref(), Some("req-42"));
        assert_eq!(ctx.request.priority, Priority::High);
        assert_eq!(ctx.attributes.get_str("experiment"), Some("control"));
        assert_eq!(
            ctx.attributes.get("attempt").and_then(|v| v.as_i64()),
            Some(3)
        );
    }

    #[test]
    fn attribute_bag_overwrites_on_set() {
        let mut bag = AttributeBag::new();
        bag.set("k", "v1");
        let prev = bag.set("k", "v2");
        assert_eq!(prev, Some(AttributeValue::String("v1".into())));
        assert_eq!(bag.get_str("k"), Some("v2"));
    }

    #[test]
    fn attribute_value_accessors_type_check() {
        let v = AttributeValue::String("hi".into());
        assert_eq!(v.as_str(), Some("hi"));
        assert_eq!(v.as_i64(), None);

        let n = AttributeValue::Int(42);
        assert_eq!(n.as_i64(), Some(42));
        assert_eq!(n.as_str(), None);

        let b = AttributeValue::Bool(true);
        assert_eq!(b.as_bool(), Some(true));
        assert_eq!(b.as_str(), None);
    }

    #[test]
    fn attribute_value_serializes_untagged() {
        // `untagged` produces the bare inner value on the wire so
        // downstream consumers can treat it as a JSON string / int / bool.
        let s = serde_json::to_string(&AttributeValue::String("hi".into())).unwrap();
        assert_eq!(s, "\"hi\"");
        let n = serde_json::to_string(&AttributeValue::Int(7)).unwrap();
        assert_eq!(n, "7");
        let b = serde_json::to_string(&AttributeValue::Bool(false)).unwrap();
        assert_eq!(b, "false");
    }

    #[test]
    fn priority_default_is_normal() {
        assert_eq!(Priority::default(), Priority::Normal);
    }

    #[test]
    fn session_context_round_trips_through_json() {
        let original = SessionContext::new("alice", "a", "s")
            .with_workspace("/tmp")
            .with_attribute("k", "v");
        let json = serde_json::to_string(&original).unwrap();
        let restored: SessionContext = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }
}
