//! Public, UI-facing service protocol.
//!
//! Transports encode these messages; they do not define competing wire types.

use serde::{Deserialize, Serialize};

use crate::{
    ApprovalScope, MessageAttachment, PermissionProfile, PlanDecision, ReasoningEffort,
    UiProtocolHello,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiClientMessage {
    Hello {
        protocol: UiProtocolHello,
    },
    Chat {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<MessageAttachment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
    },
    Approve {
        #[serde(default)]
        session_id: String,
        call_id: String,
        approved: bool,
        #[serde(default)]
        scope: ApprovalScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Answer {
        #[serde(default)]
        session_id: String,
        call_id: String,
        answer: String,
    },
    Interrupt {
        session_id: String,
    },
    ResolvePlan {
        #[serde(default)]
        session_id: String,
        plan_id: String,
        decision: PlanDecision,
    },
    CancelTask {
        session_id: String,
        task_id: String,
    },
    ListSessions,
    LoadSession {
        session_id: String,
    },
    ReattachSession {
        session_id: String,
    },
    RenameSession {
        session_id: String,
        label: String,
    },
    ArchiveSession {
        session_id: String,
    },
    RestoreSession {
        session_id: String,
    },
    DeleteSession {
        session_id: String,
    },
    ForkSession {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completed_turns: Option<usize>,
        #[serde(default)]
        checkpoint: bool,
    },
    GetRuntimeInfo,
    GetContext {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    Compact {
        session_id: String,
    },
    PreviewWorkspaceRollback {
        session_id: String,
    },
    RollbackWorkspace {
        session_id: String,
        expected_turn_id: String,
    },
    SelectModel {
        model: String,
        reasoning_effort: ReasoningEffort,
    },
    SelectPermissions {
        profile: PermissionProfile,
    },
    Ping,
}

#[cfg(test)]
mod tests {
    use super::UiClientMessage;

    #[test]
    fn legacy_chat_defaults_remain_compatible() {
        let message: UiClientMessage =
            serde_json::from_str(r#"{"type":"chat","text":"hello"}"#).unwrap();
        assert!(matches!(
            message,
            UiClientMessage::Chat {
                attachments,
                session_id: None,
                workspace: None,
                ..
            } if attachments.is_empty()
        ));
    }
}
