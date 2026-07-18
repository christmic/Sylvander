use super::*;
use crate::event::DomainEvent;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[test]
fn transcript_window_is_bounded_by_entries_and_bytes() {
    let mut state = AppState::new();
    for index in 0..(MAX_TRANSCRIPT_ENTRIES + 100) {
        state.messages.push(ChatMessage::Agent(format!(
            "{index}:{}",
            "x".repeat(10 * 1024)
        )));
    }
    state.enforce_memory_budget();

    assert!(state.messages.len() <= MAX_TRANSCRIPT_ENTRIES);
    assert!(state.messages.iter().map(message_bytes).sum::<usize>() <= MAX_TRANSCRIPT_BYTES);
    assert!(matches!(
        state.messages.first(),
        Some(ChatMessage::Info(text)) if text == TRANSCRIPT_PRUNED_NOTICE
    ));
}

#[test]
fn streaming_and_tool_payloads_are_utf8_safe_and_bounded() {
    let mut state = AppState::new();
    state.apply(DomainEvent::TextChunk {
        delta: "蟹".repeat(MAX_MESSAGE_BYTES),
    });
    assert!(state.streaming.len() <= MAX_MESSAGE_BYTES);
    assert!(state.streaming.is_char_boundary(state.streaming.len()));

    state.apply(DomainEvent::ToolStarted {
        call_id: "call-1".into(),
        tool_name: "test".into(),
        input: serde_json::json!({"payload": "x".repeat(MAX_TOOL_PAYLOAD_BYTES)}),
    });
    state.apply(DomainEvent::ToolOutputDelta {
        call_id: "call-1".into(),
        tool_name: "test".into(),
        delta: "蟹".repeat(MAX_TOOL_PAYLOAD_BYTES),
    });
    let Some(ChatMessage::ToolStep { children, .. }) = state.messages.last() else {
        panic!("tool step");
    };
    assert!(json_bytes(&children[0].input) <= MAX_TOOL_PAYLOAD_BYTES);
    assert!(
        children[0]
            .output
            .as_ref()
            .is_some_and(|output| output.len() <= MAX_TOOL_PAYLOAD_BYTES)
    );
}

#[test]
fn connection_requests_and_applies_runtime_truth() {
    let mut state = AppState::new();
    assert!(matches!(
        state.apply(DomainEvent::Connected),
        Some(Action::RequestRuntimeInfo)
    ));
    state.apply(DomainEvent::RuntimeInfo {
        model: "claude-test".into(),
        reasoning_effort: sylvander_protocol::ReasoningEffort::Low,
        models: vec![sylvander_protocol::ModelDescriptor {
            id: "claude-test".into(),
            provider: "test".into(),
            capabilities: 0b10001,
            capability_names: Vec::new(),
            reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
            lifecycle: sylvander_protocol::ModelLifecycle::Active,
            pricing: None,
        }],
        permissions: sylvander_protocol::PermissionProfile {
            file_access: sylvander_protocol::FileAccess::ReadOnly,
            network_access: sylvander_protocol::NetworkAccess::Denied,
            approval_policy: sylvander_protocol::ApprovalPolicy::Ask,
        },
        capabilities: 0b10001,
        approval_enabled: true,
        max_attachment_bytes: 4096,
        platform: sylvander_protocol::PlatformSnapshot::default(),
    });
    assert_eq!(state.metadata.model, "claude-test");
    assert_eq!(
        state.metadata.reasoning_effort,
        sylvander_protocol::ReasoningEffort::Low
    );
    assert_eq!(state.metadata.models.len(), 1);
    assert_eq!(
        state.metadata.permissions.file_access,
        sylvander_protocol::FileAccess::ReadOnly
    );
    assert_eq!(state.metadata.capabilities, 0b10001);
    assert!(state.metadata.approval_enabled);
    assert_eq!(state.metadata.max_attachment_bytes, 4096);
}

#[test]
fn protocol_negotiation_records_server_truth() {
    let mut state = AppState::new();
    let action = state.apply(DomainEvent::ProtocolNegotiated {
        version: 1,
        server_name: "test-server".into(),
        capabilities: vec!["diagnostics".into()],
    });
    assert!(matches!(action, Some(Action::RequestRuntimeInfo)));
    assert!(state.connected);
    assert_eq!(state.protocol_version, Some(1));
    assert_eq!(state.protocol_capabilities, ["diagnostics"]);
    assert!(state.status.contains("test-server"));
}

#[test]
fn reconnect_requests_reconciliation_and_preserves_the_local_queue() {
    let mut state = AppState::new();
    state.session_id = Some("session-1".into());
    state.queued_prompts.push_back("follow up".into());
    let action = state.apply(DomainEvent::ProtocolNegotiated {
        version: 1,
        server_name: "test-server".into(),
        capabilities: vec!["session_replay".into()],
    });
    assert!(matches!(
        action,
        Some(Action::ReconcileSession { session_id }) if session_id == "session-1"
    ));
    assert!(matches!(
        state.pending_actions.as_slice(),
        [Action::DiscoverAgents, Action::RequestRuntimeInfo]
    ));

    state.apply(DomainEvent::SessionHistoryLoaded {
        session: crate::model::SessionSummary {
            id: "session-1".into(),
            label: "Recovered".into(),
            workspace: "/workspace/project".into(),
            last_seen_secs: 0,
        },
        messages: vec![crate::model::HistoryEntry {
            role: HistoryRole::User,
            text: "active prompt".into(),
        }],
        iterations: 1,
        input_tokens: 10,
        output_tokens: 0,
        cost_nano_usd: Some(0),
        notice: None,
        source_session_id: None,
        recovery: true,
        replay_truncated: false,
    });
    assert_eq!(
        state.queued_prompts.front().map(String::as_str),
        Some("follow up")
    );
    assert!(
        matches!(state.messages.last(), Some(ChatMessage::QueuedUser(text)) if text == "follow up")
    );
    assert_eq!(state.status, "Reattached Recovered");
}

#[test]
fn current_deprecated_model_surfaces_migration_target() {
    let mut state = AppState::new();
    state.apply(DomainEvent::RuntimeInfo {
        model: "old-model".into(),
        reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
        models: vec![sylvander_protocol::ModelDescriptor {
            id: "old-model".into(),
            provider: "test".into(),
            capabilities: 0,
            capability_names: Vec::new(),
            reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
            lifecycle: sylvander_protocol::ModelLifecycle::Deprecated {
                replacement: Some("new-model".into()),
            },
            pricing: None,
        }],
        permissions: sylvander_protocol::PermissionProfile::default(),
        capabilities: 0,
        approval_enabled: false,
        max_attachment_bytes: 4096,
        platform: sylvander_protocol::PlatformSnapshot::default(),
    });
    assert_eq!(state.status, "Model deprecated · old-model → new-model");
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(message)) if message.contains("old-model → new-model")
    ));
}

#[test]
fn context_report_renders_provider_usage_cache_and_sources() {
    let mut state = AppState::new();
    state.apply(DomainEvent::ContextReported {
        report: sylvander_protocol::ContextReport {
            model: "deep-code".into(),
            context_window: 200_000,
            used_tokens: 50_000,
            remaining_tokens: 150_000,
            cache_read_tokens: 40_000,
            cache_write_tokens: 2_000,
            sources: vec![sylvander_protocol::ContextSource {
                kind: sylvander_protocol::ContextSourceKind::Conversation,
                label: "conversation messages".into(),
                items: 8,
            }],
        },
    });
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(text))
            if text.contains("50000 / 200000 tokens (25%)")
                && text.contains("40000 read")
                && text.contains("conversation messages (8)")
    ));
}

#[test]
fn compaction_lifecycle_is_visible_with_a_bounded_summary() {
    let mut state = AppState::new();
    state.apply(DomainEvent::CompactionStarted { automatic: false });
    assert_eq!(state.status, "Compacting context…");
    state.apply(DomainEvent::CompactionCompleted {
        report: sylvander_protocol::CompactionReport {
            automatic: false,
            removed_messages: 12,
            condensed_blocks: 3,
            freed_tokens: 4_200,
            summary: Some("Kept the architecture decisions and pending tests".into()),
        },
    });
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(text))
            if text.contains("12 messages removed")
                && text.contains("~4200 tokens freed")
                && text.contains("architecture decisions")
    ));
}

#[test]
fn composer_copy_and_cut_use_local_clipboard_effects() {
    let mut state = AppState::new();
    for character in "hello".chars() {
        state
            .composer
            .handle_key(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    state
        .composer
        .handle_key(&KeyEvent::new(KeyCode::Home, KeyModifiers::SHIFT));
    assert!(matches!(
        state.handle_key(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Some(Action::CopyText { text }) if text == "hello"
    ));
    state
        .composer
        .handle_key(&KeyEvent::new(KeyCode::End, KeyModifiers::SHIFT));
    assert!(matches!(
        state.handle_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
        Some(Action::CopyText { text }) if text == "hello"
    ));
    assert!(state.composer.is_empty());
}

#[test]
fn apply_text_chunks_accumulate_into_streaming() {
    let mut s = AppState::new();
    s.apply(DomainEvent::TextChunk {
        delta: "hel".into(),
    });
    s.apply(DomainEvent::TextChunk {
        delta: "lo!".into(),
    });
    assert_eq!(s.streaming, "hello!");
    assert!(s.messages.is_empty());
}

#[test]
fn model_retry_is_visible_and_bounded_in_transcript() {
    let mut state = AppState::new();
    state.apply(DomainEvent::ModelRetry {
        attempt: 1,
        max_attempts: 3,
        delay_ms: 100,
        reason: format!("provider unavailable {}", "x".repeat(200)),
        cause: sylvander_protocol::RetryCause::RateLimit,
    });
    assert_eq!(state.status, "Rate limited · retry 1/3");
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(text))
            if text.starts_with("Rate limited · retry 1/3 in 100ms")
                && text.chars().count() < 170
    ));
}

#[test]
fn usage_updates_cost_and_formats_sub_cent_amounts() {
    let mut state = AppState::new();
    state.apply(DomainEvent::UsageUpdated {
        iteration: 2,
        input_tokens: 1_000,
        output_tokens: 100,
        cost_nano_usd: Some(7_500_000),
    });
    assert_eq!(state.cost_nano_usd, Some(7_500_000));
    assert_eq!(format_cost(7_500_000), "$0.007500");
}

#[test]
fn rollback_lifecycle_requires_preview_and_reports_restored_files() {
    let mut state = AppState::new();
    state.apply(DomainEvent::WorkspaceRollbackPreviewed {
        session_id: "s1".into(),
        preview: sylvander_protocol::WorkspaceRollbackPreview {
            turn_id: "turn-1".into(),
            files: vec!["src/lib.rs".into()],
        },
    });
    assert_eq!(
        state.modals.top().map(crate::modal::Modal::title),
        Some("Rollback files")
    );
    state.apply(DomainEvent::WorkspaceRollbackCompleted {
        report: sylvander_protocol::WorkspaceRollbackReport {
            turn_id: "turn-1".into(),
            restored: vec!["src/lib.rs".into()],
        },
    });
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(text))
            if text.contains("src/lib.rs") && text.contains("conversation history unchanged")
    ));
}

#[test]
fn workspace_review_sends_one_typed_diff_attachment() {
    let mut state = AppState::new();
    state.session_id = Some("s1".into());
    let action = state.apply(DomainEvent::WorkspaceReviewLoaded {
        scope: crate::event::WorkspaceDiffScope::Staged,
        diff: "diff --git a/a.rs b/a.rs\n+fixed\n".into(),
    });
    let Some(Action::SendChat {
        text,
        attachments,
        session_id,
        ..
    }) = action
    else {
        panic!("review send action");
    };
    assert!(text.contains("actionable findings first"));
    assert_eq!(session_id.as_deref(), Some("s1"));
    assert!(matches!(
        attachments.as_slice(),
        [sylvander_protocol::MessageAttachment {
            kind: sylvander_protocol::AttachmentKind::Diff,
            content: sylvander_protocol::AttachmentContent::Text { text },
            ..
        }] if text.contains("+fixed")
    ));
    assert!(state.turn_active);
    assert!(matches!(state.messages.last(), Some(ChatMessage::User(_))));
}

#[test]
fn apply_agent_done_promotes_streaming_to_messages() {
    let mut s = AppState::new();
    s.apply(DomainEvent::TextChunk { delta: "hi".into() });
    s.apply(DomainEvent::AgentDone {
        final_text: "hi".into(),
    });
    assert_eq!(s.streaming, "");
    assert_eq!(s.messages.len(), 1);
    assert!(matches!(s.messages[0], ChatMessage::Agent(ref t) if t == "hi"));
}

#[test]
fn apply_agent_done_with_empty_streaming_uses_final_text() {
    let mut s = AppState::new();
    s.apply(DomainEvent::AgentDone {
        final_text: "bye".into(),
    });
    assert_eq!(s.messages.len(), 1);
}

#[test]
fn apply_tool_started_then_finished_groups_into_step() {
    // Per UX §6 / M-T14.E: consecutive `ToolStarted` + `ToolFinished`
    // events fold into a single `ToolStep` block, not two flat rows.
    // The reducer stores the children inside the step and updates
    // the child's status when the finish lands.
    let mut s = AppState::new();
    s.apply(DomainEvent::ToolStarted {
        call_id: "call-1".into(),
        tool_name: "bash".into(),
        input: serde_json::json!({"cmd": "ls"}),
    });
    assert_eq!(s.messages.len(), 1);
    match &s.messages[0] {
        ChatMessage::ToolStep { name, children, .. } => {
            assert!(name.starts_with("Run"));
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].name, "bash");
            assert_eq!(children[0].status, ToolStatus::Pending);
        }
        other => panic!("expected ToolStep, got {other:?}"),
    }
    s.apply(DomainEvent::ToolFinished {
        call_id: "call-1".into(),
        tool_name: "bash".into(),
        output: "a.txt".into(),
        is_error: false,
    });
    // Same single step; child status flipped to Done; output captured.
    match &s.messages[0] {
        ChatMessage::ToolStep { children, .. } => {
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].status, ToolStatus::Done);
            assert_eq!(children[0].output.as_deref(), Some("a.txt"));
            assert_eq!(children[0].is_error, Some(false));
        }
        other => panic!("expected ToolStep, got {other:?}"),
    }
}

#[test]
fn apply_two_separate_tools_open_then_close_separate_steps() {
    // A text chunk between two tools should close the first step
    // and open a second one. We simulate by inserting the
    // finalize moment via a manual transition (AgentDone). For
    // now we only verify that two distinct ToolStarted events
    // append two children to the SAME step (since no AgentDone
    // has landed between them) — the renderer collapses them into
    // one step group, exactly the §6 immersive behavior.
    let mut s = AppState::new();
    s.apply(DomainEvent::ToolStarted {
        call_id: "call-1".into(),
        tool_name: "bash".into(),
        input: serde_json::json!({"command": "ls src"}),
    });
    s.apply(DomainEvent::ToolFinished {
        call_id: "call-1".into(),
        tool_name: "bash".into(),
        output: "a.rs".into(),
        is_error: false,
    });
    s.apply(DomainEvent::ToolStarted {
        call_id: "call-2".into(),
        tool_name: "read".into(),
        input: serde_json::json!({"path": "src/a.rs"}),
    });
    match &s.messages[0] {
        ChatMessage::ToolStep { children, .. } => {
            assert_eq!(children.len(), 2);
            assert_eq!(children[0].name, "bash");
            assert_eq!(children[0].status, ToolStatus::Done);
            assert_eq!(children[1].name, "read");
            assert_eq!(children[1].status, ToolStatus::Pending);
        }
        other => panic!("expected ToolStep, got {other:?}"),
    }
}

#[test]
fn same_named_tool_results_match_by_call_id() {
    let mut state = AppState::new();
    for call_id in ["first", "second"] {
        state.apply(DomainEvent::ToolStarted {
            call_id: call_id.into(),
            tool_name: "read".into(),
            input: serde_json::json!({"path": format!("{call_id}.rs")}),
        });
    }
    state.apply(DomainEvent::ToolFinished {
        call_id: "first".into(),
        tool_name: "read".into(),
        output: "first result".into(),
        is_error: false,
    });

    let Some(ChatMessage::ToolStep { children, .. }) = state.messages.last() else {
        panic!("expected tool step");
    };
    assert_eq!(children[0].output.as_deref(), Some("first result"));
    assert!(children[1].output.is_none());
}

#[test]
fn partial_tool_output_appends_to_the_matching_pending_call() {
    let mut state = AppState::new();
    state.apply(DomainEvent::ToolStarted {
        call_id: "call-1".into(),
        tool_name: "read".into(),
        input: serde_json::json!({"path": "a.rs"}),
    });
    for delta in ["first ", "second"] {
        state.apply(DomainEvent::ToolOutputDelta {
            call_id: "call-1".into(),
            tool_name: "read".into(),
            delta: delta.into(),
        });
    }
    let ChatMessage::ToolStep { children, .. } = state.messages.last().unwrap() else {
        panic!("expected tool step");
    };
    assert_eq!(children[0].status, ToolStatus::Pending);
    assert_eq!(children[0].output.as_deref(), Some("first second"));
}

#[test]
fn pending_tool_output_keeps_a_utf8_safe_recent_tail() {
    let mut state = AppState::new();
    state.apply(DomainEvent::ToolStarted {
        call_id: "call-1".into(),
        tool_name: "Command".into(),
        input: serde_json::json!({"command": "long-build"}),
    });
    state.apply(DomainEvent::ToolOutputDelta {
        call_id: "call-1".into(),
        tool_name: "Command".into(),
        delta: format!("old-line\n{}", "蟹".repeat(MAX_TOOL_PAYLOAD_BYTES)),
    });
    state.apply(DomainEvent::ToolOutputDelta {
        call_id: "call-1".into(),
        tool_name: "Command".into(),
        delta: "\nlatest-line".into(),
    });

    let ChatMessage::ToolStep { children, .. } = state.messages.last().unwrap() else {
        panic!("expected tool step");
    };
    let output = children[0].output.as_deref().expect("live output");
    assert!(output.len() <= MAX_TOOL_PAYLOAD_BYTES);
    assert!(output.is_char_boundary(output.len()));
    assert!(output.starts_with("… earlier live output omitted …\n"));
    assert!(!output.contains("old-line"));
    assert!(output.ends_with("latest-line"));
}

#[test]
fn orphan_unknown_tool_events_synthesize_one_visible_step() {
    let mut state = AppState::new();
    state.apply(DomainEvent::ToolOutputDelta {
        call_id: "future-call".into(),
        tool_name: "future_extension_tool".into(),
        delta: "partial ".into(),
    });
    state.apply(DomainEvent::ToolFinished {
        call_id: "future-call".into(),
        tool_name: "future_extension_tool".into(),
        output: "complete result".into(),
        is_error: false,
    });

    let Some(ChatMessage::ToolStep { name, children, .. }) = state.messages.last() else {
        panic!("unknown tool must remain visible");
    };
    assert_eq!(name, "future_extension_tool");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].status, ToolStatus::Done);
    assert_eq!(children[0].output.as_deref(), Some("complete result"));
}

#[test]
fn orphan_unknown_tool_result_is_not_silently_dropped() {
    let mut state = AppState::new();
    state
        .messages
        .push(ChatMessage::Agent("Earlier output".into()));
    state.apply(DomainEvent::ToolFinished {
        call_id: "future-call".into(),
        tool_name: "future_extension_tool".into(),
        output: "result without start".into(),
        is_error: true,
    });

    let Some(ChatMessage::ToolStep { children, .. }) = state.messages.last() else {
        panic!("orphan result must synthesize a tool step");
    };
    assert_eq!(children[0].status, ToolStatus::Error);
    assert_eq!(children[0].input, serde_json::Value::Null);
    assert_eq!(children[0].output.as_deref(), Some("result without start"));
}

#[test]
fn apply_approval_request_pushes_modal() {
    let mut s = AppState::new();
    s.apply(DomainEvent::ApprovalRequested {
        batch_id: "b1".into(),
        allowed_scopes: vec![sylvander_protocol::ApprovalScope::Once],
        tools: vec![ToolInfo {
            call_id: "c1".into(),
            tool_name: "bash".into(),
            input: serde_json::json!({}),
        }],
    });
    assert_eq!(s.modals.len(), 1);
    assert_eq!(s.mode, AppMode::ApprovalPending);
}

#[test]
fn full_decision_stack_rejects_approval_and_unblocks_the_agent() {
    let mut state = AppState::new();
    for index in 0..64 {
        assert!(state.modals.push(Box::new(crate::modal::ApprovalModal::new(
            format!("batch-{index}"),
            Vec::new(),
        ))));
    }
    state.apply(DomainEvent::ApprovalRequested {
        batch_id: "overflow".into(),
        tools: vec![ToolInfo {
            call_id: "call-1".into(),
            tool_name: "write".into(),
            input: serde_json::json!({"path": "notes.md"}),
        }],
        allowed_scopes: vec![sylvander_protocol::ApprovalScope::Once],
    });

    assert!(matches!(
        state.pending_actions.last(),
        Some(Action::SendApprove {
            call_id,
            approved: false,
            reason: Some(reason),
            ..
        }) if call_id == "call-1" && reason == "TUI decision queue is full"
    ));
}

#[test]
fn decision_timeout_closes_stale_modal_and_explains_recovery() {
    let mut state = AppState::new();
    state.apply(DomainEvent::ApprovalRequested {
        batch_id: "batch-1".into(),
        allowed_scopes: vec![sylvander_protocol::ApprovalScope::Once],
        tools: vec![ToolInfo {
            call_id: "call-123456".into(),
            tool_name: "bash".into(),
            input: serde_json::json!({"command":"cargo test"}),
        }],
    });
    state.apply(DomainEvent::InteractionTimedOut {
        kind: sylvander_protocol::InteractionTimeoutKind::Approval,
        subject_id: "call-123456".into(),
        timeout_secs: 120,
        recovery: sylvander_protocol::TimeoutRecovery::RetryRequest,
    });
    assert!(state.modals.is_empty());
    assert_eq!(state.mode, AppMode::Normal);
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(message))
            if message.contains("approval") && message.contains("120s") && message.contains("retry")
    ));
}

#[test]
fn apply_connected_then_disconnected() {
    let mut s = AppState::new();
    s.apply(DomainEvent::Connected);
    assert!(s.connected);
    s.apply(DomainEvent::Disconnected {
        reason: "lost".into(),
    });
    assert!(!s.connected);
}

#[test]
fn apply_marks_dirty() {
    let mut s = AppState::new();
    s.dirty.take(); // clear
    s.apply(DomainEvent::Connected);
    assert!(s.dirty.is_set());
}

#[test]
fn plain_enter_submits_chat_returns_send_action() {
    let mut s = AppState::new();
    s.session_id = Some("session-1".into());
    let key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
    s.handle_key(&key);
    let key = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
    s.handle_key(&key);
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let action = s.handle_key(&enter);
    assert!(matches!(action, Some(Action::SendChat { ref text, .. }) if text == "hi"));
    assert!(s.composer.is_empty());
}

#[test]
fn submitted_prompt_is_visible_before_the_server_replies() {
    let mut s = AppState::new();
    s.session_id = Some("session-1".into());
    s.handle_key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    s.handle_key(&KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    let action = s.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(action, Some(Action::SendChat { .. })));
    assert!(matches!(s.messages.last(), Some(ChatMessage::User(text)) if text == "hi"));
}

#[test]
fn shift_enter_inserts_newline_and_does_not_submit() {
    let mut s = AppState::new();
    s.session_id = Some("session-1".into());
    s.handle_key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    s.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    s.handle_key(&KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    let action = s.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        action,
        Some(Action::SendChat { ref text, .. }) if text == "h\ni"
    ));
}

#[test]
fn esc_quits_when_no_modal() {
    let mut s = AppState::new();
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    s.handle_key(&esc);
    assert!(s.should_quit);
}

#[test]
fn vim_insert_escape_changes_mode_before_idle_exit() {
    let mut state = AppState::new();
    state
        .composer
        .set_editing_style(crate::input::EditingStyle::Vim);
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

    state.handle_key(&esc);
    assert!(!state.should_quit);
    assert_eq!(state.composer.mode_label(), Some("NORMAL"));

    state.handle_key(&esc);
    assert!(state.should_quit);
}

#[test]
fn esc_interrupts_active_turn_without_quitting() {
    let mut state = AppState::new();
    state.session_id = Some("session-1".into());
    state.turn_active = true;

    state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(!state.should_quit);
    assert!(state.interrupt_requested);
    assert!(matches!(
        state.pending_actions.as_slice(),
        [Action::InterruptTurn { session_id }] if session_id == "session-1"
    ));
}

#[test]
fn interrupted_turn_settles_partial_output_and_pending_tools() {
    let mut state = AppState::new();
    state.turn_active = true;
    state.streaming = "partial answer".into();
    state.messages.push(ChatMessage::ToolStep {
        name: "Read file".into(),
        started_at_secs: 0,
        children: vec![ToolStepChild {
            call_id: "call-1".into(),
            name: "Read".into(),
            status: ToolStatus::Pending,
            input: serde_json::json!({"path": "README.md"}),
            output: None,
            is_error: None,
        }],
    });

    state.apply(DomainEvent::TurnInterrupted {
        reason: "interrupted by user".into(),
    });

    assert!(!state.turn_active);
    assert!(
        state
            .messages
            .iter()
            .any(|message| matches!(message, ChatMessage::Agent(text) if text == "partial answer"))
    );
    assert!(state.messages.iter().any(|message| matches!(
        message,
        ChatMessage::ToolStep { children, .. }
            if children[0].status == ToolStatus::Error
    )));
}

#[test]
fn submit_during_active_turn_queues_without_sending_concurrently() {
    let mut state = AppState::new();
    state.turn_active = true;
    for character in "next request".chars() {
        state.handle_key(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }

    let action = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(action.is_none());
    assert_eq!(
        state.queued_prompts.front().map(String::as_str),
        Some("next request")
    );
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::QueuedUser(text)) if text == "next request"
    ));
}

#[test]
fn full_prompt_queue_preserves_the_unsent_composer_draft() {
    let mut state = AppState::new();
    state.turn_active = true;
    for index in 0..MAX_QUEUED_PROMPTS {
        state.queued_prompts.push_back(format!("queued {index}"));
        state.queued_prompt_attachments.push_back(Vec::new());
    }
    state.handle_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

    let action = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(action.is_none());
    assert_eq!(state.queued_prompts.len(), MAX_QUEUED_PROMPTS);
    assert_eq!(state.composer.text(), "x");
    assert!(state.status.contains("current draft preserved"));
}

#[test]
fn backspace_after_command_trigger_returns_to_the_composer() {
    let mut state = AppState::new();

    state.handle_key(&KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    assert_eq!(state.modals.len(), 1);

    state.handle_key(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(state.modals.is_empty());
    assert!(state.composer.is_empty());
    assert_eq!(state.mode, AppMode::Normal);
}

#[test]
fn persisted_session_history_replaces_the_visible_transcript() {
    let mut state = AppState::new();
    state.messages.push(ChatMessage::User("old session".into()));
    state.apply(DomainEvent::SessionHistoryLoaded {
        session: crate::model::SessionSummary {
            id: "s2".into(),
            label: "Restored".into(),
            workspace: "/workspace/project".into(),
            last_seen_secs: 1,
        },
        messages: vec![
            crate::model::HistoryEntry {
                role: HistoryRole::User,
                text: "restored question".into(),
            },
            crate::model::HistoryEntry {
                role: HistoryRole::Assistant,
                text: "restored answer".into(),
            },
        ],
        iterations: 4,
        input_tokens: 800,
        output_tokens: 120,
        cost_nano_usd: Some(7_500_000),
        notice: None,
        source_session_id: None,
        recovery: false,
        replay_truncated: false,
    });

    assert_eq!(state.session_id.as_deref(), Some("s2"));
    assert_eq!(state.messages.len(), 2);
    assert!(
        matches!(state.messages[0], ChatMessage::User(ref text) if text == "restored question")
    );
    assert!(matches!(state.messages[1], ChatMessage::Agent(ref text) if text == "restored answer"));
    assert_eq!(
        (state.iteration, state.input_tokens, state.output_tokens),
        (4, 800, 120)
    );
    assert_eq!(state.cost_nano_usd, Some(7_500_000));
}

#[test]
fn rewind_notice_is_kept_in_the_restored_transcript() {
    let mut state = AppState::new();
    state.apply(DomainEvent::SessionHistoryLoaded {
        session: crate::model::SessionSummary {
            id: "rewind-1".into(),
            label: "Work (rewind 1)".into(),
            workspace: "/workspace/project".into(),
            last_seen_secs: 1,
        },
        messages: Vec::new(),
        iterations: 0,
        input_tokens: 0,
        output_tokens: 0,
        cost_nano_usd: Some(0),
        notice: Some("Conversation rewound · workspace files unchanged".into()),
        source_session_id: Some("source-1".into()),
        recovery: false,
        replay_truncated: false,
    });
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(text)) if text.contains("workspace files unchanged")
    ));
    assert_eq!(
        state.last_branch_source_session_id.as_deref(),
        Some("source-1")
    );
}

#[test]
fn esc_dismisses_modal_first() {
    let mut s = AppState::new();
    s.apply(DomainEvent::ApprovalRequested {
        batch_id: "b".into(),
        allowed_scopes: vec![sylvander_protocol::ApprovalScope::Once],
        tools: vec![ToolInfo {
            call_id: "c".into(),
            tool_name: "bash".into(),
            input: serde_json::json!({}),
        }],
    });
    assert!(!s.modals.is_empty());
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    s.handle_key(&esc);
    assert!(s.modals.is_empty());
    assert!(!s.should_quit);
}

#[test]
fn approval_y_sends_approve_action() {
    let mut s = AppState::new();
    s.apply(DomainEvent::ApprovalRequested {
        batch_id: "b".into(),
        allowed_scopes: vec![sylvander_protocol::ApprovalScope::Once],
        tools: vec![ToolInfo {
            call_id: "c1".into(),
            tool_name: "bash".into(),
            input: serde_json::json!({}),
        }],
    });
    let y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
    s.handle_key(&y);
    assert!(s.modals.is_empty());
    assert_eq!(s.pending_actions.len(), 1);
    assert!(matches!(
        s.pending_actions[0],
        Action::SendApprove { ref call_id, approved: true, .. } if call_id == "c1"
    ));
}

#[test]
fn ctrl_p_pushes_sessions_overlay() {
    let mut s = AppState::new();
    let key = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
    s.handle_key(&key);
    assert_eq!(s.modals.len(), 1);
    // Press Ctrl+P again — top is overlay, which handles its own keys.
    s.handle_key(&key);
    // Overlay's handler closes on Ctrl+P.
    assert!(s.modals.is_empty());
}

#[test]
fn transcript_navigation_detaches_and_returns_to_live() {
    let mut s = AppState::new();
    s.set_chat_scroll_limit(80);
    s.handle_key(&KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    assert_eq!(s.chat_scroll, 8);

    s.apply(DomainEvent::TextChunk {
        delta: "new output".into(),
    });
    assert_eq!(s.chat_scroll, 8, "streaming must not steal the viewport");
    assert_eq!(s.unread_events, 1);

    s.handle_key(&KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
    assert_eq!(s.chat_scroll, 0);
    assert_eq!(s.unread_events, 0);
}

#[test]
fn ctrl_end_returns_directly_to_live() {
    let mut s = AppState::new();
    s.chat_scroll = 40;
    s.unread_events = 7;
    s.handle_key(&KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
    assert_eq!(s.chat_scroll, 0);
    assert_eq!(s.unread_events, 0);
}

#[test]
fn idle_tick_does_not_schedule_a_repaint() {
    let mut s = AppState::new();
    assert!(s.dirty.take(), "initial frame must render");
    s.apply(DomainEvent::Tick);
    assert!(!s.dirty.take(), "idle terminal must remain still");

    s.streaming.push_str("working");
    s.apply(DomainEvent::Tick);
    assert!(s.dirty.take(), "live output may animate on a tick");
}

#[test]
fn session_created_populates_sessions_cache() {
    let mut s = AppState::new();
    s.apply(DomainEvent::SessionCreated {
        session_id: "abc-123".into(),
        config: None,
    });
    assert_eq!(s.sessions.len(), 1);
    assert_eq!(s.sessions[0].id, "abc-123");
    assert_eq!(s.session_id.as_deref(), Some("abc-123"));
    // Re-creating the same id should NOT add a dup row.
    s.apply(DomainEvent::SessionCreated {
        session_id: "abc-123".into(),
        config: None,
    });
    assert_eq!(s.sessions.len(), 1);
}

#[test]
fn first_prompt_creates_configured_session_then_sends_exactly_once() {
    let mut state = AppState::with_metadata(
        None,
        RuntimeMetadata {
            workspace: "/tmp/work".into(),
            ..RuntimeMetadata::default()
        },
    );
    state.apply(DomainEvent::AgentsDiscovered {
        agents: vec![sylvander_protocol::AgentDescriptor {
            id: sylvander_protocol::AgentId::new("coding"),
            revision: 1,
            name: "Coding".into(),
            provider_id: "provider".into(),
            default_model_id: "default".into(),
            models: Vec::new(),
            default_prompt_profile: None,
            agent_workspace: None,
        }],
    });
    state.session_model_override = Some((
        sylvander_protocol::ModelSelection {
            provider_id: "provider".into(),
            model_id: "fast".into(),
        },
        sylvander_protocol::ReasoningEffort::Low,
    ));

    let create = state
        .submit_prompt("ship it".into(), Vec::new())
        .expect("explicit session creation");
    assert!(matches!(
        create,
        Action::CreateSession { request }
            if request.agent_id.0 == "coding"
                && request.overrides.model.as_ref().is_some_and(|model| model.model_id == "fast")
                && request.overrides.user_workspace.as_ref().is_some_and(|workspace| workspace.path == std::path::Path::new("/tmp/work"))
    ));
    assert!(state.session_creation_pending);
    let repeated_agents = state.agents.clone();
    assert!(
        state
            .apply(DomainEvent::AgentsDiscovered {
                agents: repeated_agents,
            })
            .is_none(),
        "a repeated discovery response must not create a second session"
    );

    let send = state
        .apply(DomainEvent::SessionCreated {
            session_id: "session-1".into(),
            config: None,
        })
        .expect("pending prompt sent after creation");
    assert!(matches!(
        send,
        Action::SendChat { text, session_id: Some(session_id), .. }
            if text == "ship it" && session_id == "session-1"
    ));
    assert!(!state.session_creation_pending);
    assert!(
        state
            .apply(DomainEvent::SessionCreated {
                session_id: "session-1".into(),
                config: None,
            })
            .is_none()
    );
}

#[test]
fn background_task_lifecycle_updates_one_stable_transcript_entry() {
    let mut s = AppState::new();
    s.apply(DomainEvent::TaskStarted {
        task_id: "task-1".into(),
        owner: "sylvander".into(),
        purpose: "Inspect tests".into(),
    });
    s.apply(DomainEvent::TaskProgress {
        task_id: "task-1".into(),
        message: "running read".into(),
    });
    s.apply(DomainEvent::TaskCompleted {
        task_id: "task-1".into(),
        summary: "No failures".into(),
    });

    let ChatMessage::TaskList { tasks } = &s.messages[0] else {
        panic!("task list");
    };
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].state, TaskState::Done);
    assert_eq!(tasks[0].detail, "No failures");
}

#[test]
fn plan_progress_updates_existing_block_without_opening_another_modal() {
    let mut s = AppState::new();
    s.messages.push(ChatMessage::Plan {
        plan_id: "plan-1".into(),
        steps: vec!["inspect".into(), "verify".into()],
        current: 0,
    });
    s.apply(DomainEvent::PlanUpdated {
        plan_id: "plan-1".into(),
        steps: vec!["inspect".into(), "verify".into()],
        current: 1,
    });
    assert_eq!(s.messages.len(), 1);
    assert!(matches!(
        s.messages[0],
        ChatMessage::Plan { current: 1, .. }
    ));
    assert!(s.modals.is_empty());
}

#[test]
fn at_sign_at_token_boundary_opens_file_picker_instead_of_mutating_draft() {
    let mut s = AppState::new();
    s.handle_key(&KeyEvent::new(KeyCode::Char('@'), KeyModifiers::NONE));
    assert_eq!(
        s.modals.top().map(crate::modal::Modal::title),
        Some("Mention file")
    );
    assert!(s.composer.is_empty());
}
