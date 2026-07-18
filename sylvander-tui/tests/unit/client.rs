use super::*;

#[tokio::test]
async fn bounded_line_reader_accepts_crlf_and_rejects_oversized_frames() {
    let mut reader = BufReader::new(&b"first\r\nsecond\n"[..]);
    assert_eq!(
        read_bounded_line(&mut reader, 16).await.unwrap(),
        Some("first".into())
    );
    assert_eq!(
        read_bounded_line(&mut reader, 16).await.unwrap(),
        Some("second".into())
    );

    let oversized = vec![b'x'; 17];
    let mut reader = BufReader::new(oversized.as_slice());
    let error = read_bounded_line(&mut reader, 16)
        .await
        .expect_err("oversized frame");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn socket_event_queue_applies_backpressure_at_its_capacity() {
    let (client, _events) = UnixClient::new("/tmp/sylvander-test.sock");
    for index in 0..CLIENT_EVENT_CAPACITY {
        client
            .event_tx
            .try_send(ClientEvent::Diagnostic(index.to_string()))
            .expect("queue slot");
    }
    assert!(matches!(
        client
            .event_tx
            .try_send(ClientEvent::Diagnostic("overflow".into())),
        Err(mpsc::error::TrySendError::Full(_))
    ));
}

#[test]
fn unknown_server_messages_produce_bounded_diagnostics() {
    let line = format!(r#"{{"type":"future_{}"}}"#, "x".repeat(500));
    let diagnostic = parse_server_line(&line).expect_err("unknown event must be visible");
    assert!(diagnostic.starts_with("Rejected server message"));
    assert!(diagnostic.chars().count() < 300);
}

#[test]
fn timeout_wire_event_preserves_recovery_contract() {
    let event = parse_server_msg(ServerMsg::InteractionTimeout {
        session_id: "session-1".into(),
        kind: sylvander_protocol::InteractionTimeoutKind::Tool,
        subject_id: "call-1".into(),
        timeout_secs: 120,
        recovery: sylvander_protocol::TimeoutRecovery::NarrowScope,
    });
    assert!(matches!(
        event,
        Some(DomainEvent::InteractionTimedOut {
            kind: sylvander_protocol::InteractionTimeoutKind::Tool,
            subject_id,
            timeout_secs: 120,
            recovery: sylvander_protocol::TimeoutRecovery::NarrowScope,
        }) if subject_id == "call-1"
    ));
}

#[test]
fn agent_discovery_crosses_the_protocol_adapter() {
    let event = parse_server_msg(ServerMsg::AgentsDiscovered {
        agents: vec![sylvander_protocol::AgentDescriptor {
            id: sylvander_protocol::AgentId::new("coding"),
            revision: 3,
            name: "Coding".into(),
            provider_id: "provider".into(),
            default_model_id: "model".into(),
            models: Vec::new(),
            default_prompt_profile: None,
            agent_workspace: None,
        }],
    });
    assert!(matches!(
        event,
        Some(DomainEvent::AgentsDiscovered { agents })
            if agents.len() == 1 && agents[0].id.0 == "coding"
    ));
}

#[test]
fn runtime_wire_event_preserves_server_capabilities() {
    let event = parse_server_msg(ServerMsg::RuntimeInfo {
        model: sylvander_protocol::ModelSelection {
            provider_id: "test".into(),
            model_id: "claude-test".into(),
        },
        reasoning_effort: sylvander_protocol::ReasoningEffort::Medium,
        models: vec![sylvander_protocol::ModelDescriptor {
            id: "claude-test".into(),
            provider: "test".into(),
            capabilities: 0b10001,
            capability_names: Vec::new(),
            reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Medium],
            lifecycle: sylvander_protocol::ModelLifecycle::Active,
            pricing: None,
        }],
        permissions: sylvander_protocol::PermissionProfile::default(),
        capabilities: 0b10001,
        approval_enabled: true,
        max_attachment_bytes: 4096,
        platform: sylvander_protocol::PlatformSnapshot::default(),
    });
    assert!(matches!(
        event,
        Some(DomainEvent::RuntimeInfo {
            model,
            reasoning_effort: sylvander_protocol::ReasoningEffort::Medium,
            models,
            capabilities: 0b10001,
            approval_enabled: true,
            max_attachment_bytes: 4096,
            ..
        }) if model == "claude-test" && models.len() == 1
    ));
}

#[test]
fn legacy_usage_event_defaults_to_unknown_cost() {
    let message: ServerMsg = serde_json::from_value(serde_json::json!({
        "type": "iteration_end",
        "session_id": "s1",
        "iteration": 1,
        "input_tokens": 10,
        "output_tokens": 2
    }))
    .expect("legacy iteration event");
    assert!(matches!(
        parse_server_msg(message),
        Some(DomainEvent::UsageUpdated {
            cost_nano_usd: None,
            ..
        })
    ));
}

#[test]
fn model_selection_uses_typed_reasoning_effort_on_wire() {
    let value = serde_json::to_value(ClientMsg::SelectModel {
        session_id: Some("session-1".into()),
        model: sylvander_protocol::ModelSelection {
            provider_id: "provider-a".into(),
            model_id: "thinking".into(),
        },
        reasoning_effort: sylvander_protocol::ReasoningEffort::High,
    })
    .unwrap();
    assert_eq!(value["type"], "select_model");
    assert_eq!(value["session_id"], "session-1");
    assert_eq!(value["model"]["provider_id"], "provider-a");
    assert_eq!(value["model"]["model_id"], "thinking");
    assert_eq!(value["reasoning_effort"], "high");
}

#[test]
fn permission_selection_is_a_typed_wire_profile() {
    let value = serde_json::to_value(ClientMsg::SelectPermissions {
        session_id: Some("session-1".into()),
        profile: sylvander_protocol::PermissionProfile {
            file_access: sylvander_protocol::FileAccess::ReadOnly,
            network_access: sylvander_protocol::NetworkAccess::Denied,
            approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
        },
    })
    .unwrap();
    assert_eq!(value["type"], "select_permissions");
    assert_eq!(value["profile"]["file_access"], "read_only");
    assert_eq!(value["profile"]["approval_policy"], "deny");
}

#[test]
fn context_report_round_trips_as_typed_server_truth() {
    let request = serde_json::to_value(ClientMsg::GetContext {
        session_id: Some("session-1".into()),
    })
    .expect("serialize");
    assert_eq!(request["type"], "get_context");
    assert_eq!(request["session_id"], "session-1");

    let event = parse_server_msg(ServerMsg::ContextReport {
        report: sylvander_protocol::ContextReport {
            model: "deep-code".into(),
            context_window: 100_000,
            used_tokens: 25_000,
            remaining_tokens: 75_000,
            cache_read_tokens: 20_000,
            cache_write_tokens: 1_000,
            sources: vec![],
        },
    });
    assert!(matches!(
        event,
        Some(DomainEvent::ContextReported { report })
            if report.used_tokens == 25_000 && report.cache_read_tokens == 20_000
    ));
}

#[test]
fn compaction_wire_lifecycle_preserves_manual_identity_and_summary() {
    let request = serde_json::to_value(ClientMsg::Compact {
        session_id: "session-1".into(),
    })
    .expect("serialize");
    assert_eq!(request["type"], "compact");
    assert_eq!(request["session_id"], "session-1");

    let event = parse_server_msg(ServerMsg::CompactionCompleted {
        session_id: "session-1".into(),
        report: sylvander_protocol::CompactionReport {
            automatic: false,
            removed_messages: 8,
            condensed_blocks: 0,
            freed_tokens: 2_000,
            summary: Some("preserved summary".into()),
        },
    });
    assert!(matches!(
        event,
        Some(DomainEvent::CompactionCompleted { report })
            if !report.automatic && report.summary.as_deref() == Some("preserved summary")
    ));
}

#[test]
fn operation_errors_do_not_impersonate_agent_failures() {
    let event = parse_server_msg(ServerMsg::OperationError {
        operation: "load_session".into(),
        message: "not found".into(),
    });
    assert!(matches!(
        event,
        Some(DomainEvent::OperationFailed { operation, message })
            if operation == "load_session" && message == "not found"
    ));
}

#[test]
fn boundary_denials_preserve_operation_and_retry_guidance() {
    let event = parse_server_msg(ServerMsg::BoundaryDenied {
        error: sylvander_protocol::BoundaryError {
            code: sylvander_protocol::BoundaryErrorCode::RateLimited,
            operation: "chat".into(),
            request_id: "request-1".into(),
            message: "request rate limit exceeded".into(),
            retry_after_ms: Some(1_500),
        },
    });
    assert!(matches!(
        event,
        Some(DomainEvent::OperationFailed { operation, message })
            if operation == "chat" && message.contains("1500 ms")
    ));
}

#[test]
fn model_retry_wire_event_preserves_backoff_context() {
    let event = parse_server_msg(ServerMsg::ModelRetry {
        session_id: "s1".into(),
        attempt: 2,
        max_attempts: 3,
        delay_ms: 200,
        reason: "rate limited".into(),
        cause: sylvander_protocol::RetryCause::RateLimit,
    });
    assert!(matches!(
        event,
        Some(DomainEvent::ModelRetry {
            attempt: 2,
            max_attempts: 3,
            delay_ms: 200,
            reason,
            cause: sylvander_protocol::RetryCause::RateLimit,
        }) if reason == "rate limited"
    ));
}

#[test]
fn tool_call_adapter_preserves_identity_and_input() {
    let event = parse_server_msg(ServerMsg::ToolCall {
        session_id: "s1".into(),
        call_id: "call-42".into(),
        tool_name: "read".into(),
        input: serde_json::json!({"path": "src/lib.rs"}),
    });
    assert!(matches!(
        event,
        Some(DomainEvent::ToolStarted { call_id, tool_name, input })
            if call_id == "call-42"
                && tool_name == "read"
                && input["path"] == "src/lib.rs"
    ));
}

#[test]
fn tool_delta_adapter_preserves_call_identity() {
    let event = parse_server_msg(ServerMsg::ToolOutputDelta {
        session_id: "s1".into(),
        call_id: "call-42".into(),
        tool_name: "read".into(),
        delta: "partial".into(),
    });
    assert!(matches!(
        event,
        Some(DomainEvent::ToolOutputDelta { call_id, tool_name, delta })
            if call_id == "call-42" && tool_name == "read" && delta == "partial"
    ));
}

#[test]
fn answer_uses_the_server_supported_wire_shape() {
    let json = serde_json::to_value(ClientMsg::Answer {
        session_id: "s1".into(),
        call_id: "c1".into(),
        answer: "blue".into(),
    })
    .unwrap();
    assert_eq!(json["type"], "answer");
    assert_eq!(json["call_id"], "c1");
    assert_eq!(json["session_id"], "s1");
}

#[test]
fn approval_rejection_reason_uses_the_typed_wire_shape() {
    let json = serde_json::to_value(ClientMsg::Approve {
        session_id: "s1".into(),
        call_id: "c1".into(),
        approved: false,
        scope: sylvander_protocol::ApprovalScope::Once,
        reason: Some("unsafe outside workspace".into()),
    })
    .unwrap();
    assert_eq!(json["type"], "approve");
    assert_eq!(json["call_id"], "c1");
    assert_eq!(json["reason"], "unsafe outside workspace");
}

#[test]
fn interrupt_is_scoped_to_one_session_on_the_wire() {
    let json = serde_json::to_value(ClientMsg::Interrupt {
        session_id: "session-7".into(),
    })
    .unwrap();
    assert_eq!(json["type"], "interrupt");
    assert_eq!(json["session_id"], "session-7");
}

#[test]
fn interrupted_wire_event_has_a_terminal_domain_state() {
    let event = parse_server_msg(ServerMsg::TurnInterrupted {
        session_id: "session-7".into(),
        reason: "interrupted by user".into(),
    });
    assert!(matches!(
        event,
        Some(DomainEvent::TurnInterrupted { reason })
            if reason == "interrupted by user"
    ));
}

#[test]
fn plan_wire_event_maps_to_review_and_resolution_is_typed() {
    let event = parse_server_msg(ServerMsg::PlanProposed {
        session_id: "s1".into(),
        plan_id: "plan-1".into(),
        steps: vec!["inspect".into(), "verify".into()],
        current: 1,
    });
    assert!(matches!(
        event,
        Some(DomainEvent::PlanReceived { plan_id, current: 1, .. })
            if plan_id == "plan-1"
    ));

    let json = serde_json::to_value(ClientMsg::ResolvePlan {
        session_id: "s1".into(),
        plan_id: "plan-1".into(),
        decision: sylvander_protocol::PlanDecision::Approved,
    })
    .expect("serialize");
    assert_eq!(json["type"], "resolve_plan");
    assert_eq!(json["decision"]["decision"], "approved");

    let update = parse_server_msg(ServerMsg::PlanUpdated {
        session_id: "s1".into(),
        plan_id: "plan-1".into(),
        steps: vec!["inspect".into(), "verify".into()],
        current: 1,
    });
    assert!(matches!(
        update,
        Some(DomainEvent::PlanUpdated { current: 1, .. })
    ));
}

#[test]
fn background_task_lifecycle_and_scoped_cancel_keep_identity() {
    let event = parse_server_msg(ServerMsg::TaskCompleted {
        session_id: "s1".into(),
        task_id: "task-42".into(),
        summary: "found it".into(),
    });
    assert!(matches!(
        event,
        Some(DomainEvent::TaskCompleted { task_id, summary })
            if task_id == "task-42" && summary == "found it"
    ));

    let json = serde_json::to_value(ClientMsg::CancelTask {
        session_id: "s1".into(),
        task_id: "task-42".into(),
    })
    .expect("serialize");
    assert_eq!(json["type"], "cancel_task");
    assert_eq!(json["session_id"], "s1");
    assert_eq!(json["task_id"], "task-42");
}

#[test]
fn chat_serializes_typed_attachments_without_text_wrappers() {
    let message = ClientMsg::Chat {
        text: "review".into(),
        attachments: vec![sylvander_protocol::MessageAttachment {
            id: "a1".into(),
            kind: sylvander_protocol::AttachmentKind::File,
            name: "src/main.rs".into(),
            mime_type: "text/x-rust".into(),
            content: sylvander_protocol::AttachmentContent::Text {
                text: "fn main() {}".into(),
            },
            byte_count: 12,
        }],
        session_id: Some("s1".into()),
        workspace: Some("/repo".into()),
    };
    let value = serde_json::to_value(message).expect("serialize");
    assert_eq!(value["attachments"][0]["name"], "src/main.rs");
    assert!(!value["text"].as_str().unwrap().contains("[attachments]"));
}

#[test]
fn persisted_history_maps_to_protocol_neutral_roles() {
    let event = parse_server_msg(ServerMsg::SessionHistory {
        session: SessionInfoMsg {
            id: "s1".into(),
            label: "Auth work".into(),
            workspace: "/workspace".into(),
            last_seen_secs: 3,
        },
        messages: vec![
            HistoryMessageMsg {
                role: "user".into(),
                text: "hello".into(),
            },
            HistoryMessageMsg {
                role: "assistant".into(),
                text: "hi".into(),
            },
        ],
        iterations: 2,
        input_tokens: 120,
        output_tokens: 30,
        cost_nano_usd: Some(45_000),
        notice: None,
        source_session_id: None,
        recovery: false,
        replay_truncated: false,
    });
    assert!(matches!(
        event,
        Some(DomainEvent::SessionHistoryLoaded {
            session,
            messages,
            iterations: 2,
            input_tokens: 120,
            output_tokens: 30,
            cost_nano_usd: Some(45_000),
            notice: None,
            source_session_id: None,
            recovery: false,
            replay_truncated: false,
        })
            if session.id == "s1"
                && messages[0].role == crate::model::HistoryRole::User
                && messages[1].role == crate::model::HistoryRole::Assistant
    ));
}
