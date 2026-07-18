use super::*;

#[test]
fn every_visible_command_and_alias_resolves_to_one_typed_effect() {
    for (index, spec) in COMMANDS.iter().enumerate() {
        assert!(!spec.name.is_empty());
        assert!(spec.usage.starts_with('/'));
        assert!(!spec.description.is_empty());
        assert_eq!(
            resolve(spec.name).map(|resolved| resolved.id),
            Some(spec.id)
        );
        assert_eq!(
            COMMANDS.iter().filter(|other| other.id == spec.id).count(),
            1,
            "duplicate command effect for {} at {index}",
            spec.name
        );
        assert_eq!(
            COMMANDS
                .iter()
                .filter(|other| other.name.eq_ignore_ascii_case(spec.name))
                .count(),
            1,
            "duplicate visible command name {}",
            spec.name
        );
    }
    for (alias, expected) in ALIASES {
        assert!(
            COMMANDS
                .iter()
                .all(|spec| !spec.name.eq_ignore_ascii_case(alias))
        );
        assert_eq!(resolve(alias).map(|spec| spec.id), Some(*expected));
    }
}

#[test]
fn parser_accepts_arguments_and_leading_slash() {
    let invocation = parse("/theme midnight").unwrap();
    assert_eq!(invocation.spec.id, CommandId::Theme);
    assert_eq!(invocation.args, vec!["midnight"]);
}

#[test]
fn registry_ranks_fuzzy_names_aliases_and_recent_successes() {
    let mut state = AppState::new();
    let fuzzy = ranked_commands("sstns", &state);
    assert_eq!(COMMANDS[fuzzy[0].index].id, CommandId::Sessions);
    assert_eq!(parse("/history").unwrap().spec.id, CommandId::Sessions);

    execute(parse("/status").unwrap(), &mut state).unwrap();
    let unfiltered = ranked_commands("", &state);
    assert_eq!(COMMANDS[unfiltered[0].index].id, CommandId::Status);
}

#[test]
fn registry_explains_commands_that_cannot_run_now() {
    let mut state = AppState::new();
    let model = resolve("model").unwrap();
    assert_eq!(
        availability(model, &state).reason(),
        Some("connect to the Agent first")
    );
    state.connected = true;
    state.turn_active = true;
    let new = resolve("new").unwrap();
    assert_eq!(
        availability(new, &state).reason(),
        Some("interrupt active work first")
    );
}

#[test]
fn new_resets_conversation_without_sending_an_empty_prompt() {
    let mut state = AppState::new();
    state.session_id = Some("old".into());
    state.messages.push(ChatMessage::User("hello".into()));
    execute(parse("new").unwrap(), &mut state).unwrap();
    assert!(state.session_id.is_none());
    assert!(state.messages.is_empty());
    assert!(state.pending_actions.is_empty());
}

#[test]
fn context_command_requests_server_truth_for_the_active_session() {
    let mut state = AppState::new();
    state.connected = true;
    state.session_id = Some("session-7".into());
    execute(parse("/context").expect("parse"), &mut state).expect("execute");
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::RequestContext { session_id }]
            if session_id.as_deref() == Some("session-7")
    ));
}

#[test]
fn profile_command_fails_closed_until_capability_negotiation() {
    let mut state = AppState::new();
    state.connected = true;
    let profile = resolve("profile").expect("profile command");
    assert_eq!(
        availability(profile, &state).reason(),
        Some("server does not advertise user_profile_v1")
    );
    state
        .protocol_capabilities
        .push(sylvander_protocol::USER_PROFILE_CAPABILITY.into());
    assert!(availability(profile, &state).is_available());
}

#[test]
fn profile_mutations_read_the_server_revision_before_editing() {
    let mut state = AppState::new();
    state.connected = true;
    state
        .protocol_capabilities
        .push(sylvander_protocol::USER_PROFILE_CAPABILITY.into());

    execute(parse("/profile edit").expect("parse"), &mut state).expect("execute");
    assert!(matches!(
        state.pending_profile_intent,
        Some(crate::app::PendingProfileIntent::Edit { correction: false })
    ));
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::UserProfile {
            request: sylvander_protocol::UserProfileRequest {
                action: sylvander_protocol::UserProfileAction::Read {},
                ..
            }
        }]
    ));
}

#[test]
fn profile_export_is_a_typed_json_request() {
    let mut state = AppState::new();
    state.connected = true;
    state
        .protocol_capabilities
        .push(sylvander_protocol::USER_PROFILE_CAPABILITY.into());

    execute(parse("/profile export").expect("parse"), &mut state).expect("execute");
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::UserProfile {
            request: sylvander_protocol::UserProfileRequest {
                action: sylvander_protocol::UserProfileAction::Export {
                    format: sylvander_protocol::UserProfileExportFormat::Json,
                },
                ..
            }
        }]
    ));
}

#[test]
fn feedback_requires_a_completed_turn_and_emits_only_the_opaque_target() {
    let mut state = AppState::new();
    state.connected = true;
    let feedback = resolve("feedback").expect("feedback command");
    assert_eq!(
        availability(feedback, &state).reason(),
        Some("server does not advertise feedback_v1")
    );
    state
        .protocol_capabilities
        .push(sylvander_protocol::FEEDBACK_CAPABILITY.into());
    assert_eq!(
        availability(feedback, &state).reason(),
        Some("complete a turn before recording feedback")
    );
    state.feedback_target = Some(sylvander_protocol::FeedbackTarget("sha256:opaque".into()));

    execute(
        parse("/feedback correction use the verified output").expect("parse"),
        &mut state,
    )
    .expect("execute");
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::SubmitFeedback {
            feedback: sylvander_protocol::RunFeedback {
                target: sylvander_protocol::FeedbackTarget(target),
                rating: sylvander_protocol::FeedbackRating::Negative,
                correction: Some(correction),
                task_result: None,
                ..
            }
        }] if target == "sha256:opaque" && correction == "use the verified output"
    ));
}

#[test]
fn feedback_note_requires_an_explicit_rating() {
    let mut state = AppState::new();
    state.connected = true;
    state
        .protocol_capabilities
        .push(sylvander_protocol::FEEDBACK_CAPABILITY.into());
    state.feedback_target = Some(sylvander_protocol::FeedbackTarget("sha256:opaque".into()));
    assert!(
        execute(parse("/feedback note useful").expect("parse"), &mut state)
            .unwrap_err()
            .contains("note <positive|negative>")
    );
    execute(
        parse("/feedback note positive concise and correct").expect("parse"),
        &mut state,
    )
    .expect("execute");
    assert!(matches!(
        state.pending_actions.last(),
        Some(crate::event::Action::SubmitFeedback {
            feedback: sylvander_protocol::RunFeedback {
                note: Some(note),
                rating: sylvander_protocol::FeedbackRating::Positive,
                ..
            }
        }) if note == "concise and correct"
    ));
}

#[test]
fn compact_command_requires_an_idle_persisted_session() {
    let mut state = AppState::new();
    state.connected = true;
    assert_eq!(
        execute(parse("/compact").expect("parse"), &mut state).unwrap_err(),
        "/compact unavailable: requires a persisted session"
    );
    state.session_id = Some("session-7".into());
    execute(parse("/compact").expect("parse"), &mut state).expect("execute");
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::CompactSession { session_id }]
            if session_id == "session-7"
    ));
}

#[test]
fn tools_argument_is_validated() {
    let mut state = AppState::new();
    execute(parse("tools expand").unwrap(), &mut state).unwrap();
    assert!(state.tool_details_expanded);
    assert!(execute(parse("tools sideways").unwrap(), &mut state).is_err());
}

#[test]
fn status_distinguishes_priced_and_unpriced_usage() {
    let mut state = AppState::new();
    state.cost_nano_usd = Some(7_500_000);
    execute(parse("status").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(text)) if text.contains("estimated cost $0.007500")
    ));
    state.cost_nano_usd = None;
    execute(parse("status").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(text)) if text.contains("cost unavailable")
    ));
}

#[test]
fn rewind_is_a_non_destructive_server_branch_action() {
    let mut state = AppState::new();
    state.connected = true;
    state.session_id = Some("session-1".into());
    execute(parse("rewind 2").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::ForkSession {
            session_id,
            completed_turns: Some(2),
            checkpoint: false,
        }] if session_id == "session-1"
    ));
    assert!(state.status.contains("workspace unchanged"));
    assert!(execute(parse("rewind 0").unwrap(), &mut state).is_err());
}

#[test]
fn checkpoint_and_undo_keep_file_safety_explicit() {
    let mut state = AppState::new();
    state.connected = true;
    state.session_id = Some("session-1".into());
    execute(parse("checkpoint").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::ForkSession {
            checkpoint: true,
            completed_turns: None,
            ..
        }]
    ));
    state.pending_actions.clear();
    state.last_branch_source_session_id = Some("session-1".into());
    execute(parse("undo").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::LoadSession { session_id }] if session_id == "session-1"
    ));
    assert!(state.status.contains("workspace files unchanged"));
}

#[test]
fn rollback_requests_server_preview_before_any_mutation() {
    let mut state = AppState::new();
    state.connected = true;
    state.session_id = Some("session-1".into());
    execute(parse("rollback").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::PreviewWorkspaceRollback { session_id }]
            if session_id == "session-1"
    ));
}

#[test]
fn editor_command_is_a_local_runtime_effect() {
    let mut state = AppState::new();
    execute(parse("editor").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::EditDraft]
    ));
}

#[test]
fn mention_command_opens_the_same_workspace_picker_as_at_sign() {
    let mut state = AppState::new();
    execute(parse("/mention").expect("parse"), &mut state).expect("execute");
    assert_eq!(
        state.modals.top().map(crate::modal::Modal::title),
        Some("Mention file")
    );
    assert!(state.pending_actions.is_empty());
}

#[test]
fn diff_command_inspects_the_server_coding_session() {
    let mut state = AppState::new();
    state.connected = true;
    state.session_id = Some("session-1".into());
    execute(parse("/diff").expect("parse"), &mut state).expect("execute");
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::InspectCodingSession { session_id }]
            if session_id == "session-1"
    ));
    assert!(execute(parse("/diff staged").expect("parse"), &mut state).is_err());
}

#[test]
fn review_command_loads_diff_only_while_idle() {
    let mut state = AppState::new();
    execute(parse("/review unstaged").expect("parse"), &mut state).expect("execute");
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::ReviewWorkspaceChanges {
            scope: crate::event::WorkspaceDiffScope::Unstaged,
            ..
        }]
    ));
    state.pending_actions.clear();
    state.turn_active = true;
    assert!(execute(parse("/review").expect("parse"), &mut state).is_err());
}

#[test]
fn config_command_is_a_read_only_local_effect() {
    let mut state = AppState::new();
    execute(parse("/config").expect("parse"), &mut state).expect("execute");
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::InspectConfig]
    ));
}

#[test]
fn doctor_export_keeps_the_explicit_destination_typed() {
    let mut state = AppState::new();
    execute(
        parse("/doctor export reports/tui.txt").expect("parse"),
        &mut state,
    )
    .expect("execute");
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::RunDoctor {
            destination: crate::event::DoctorDestination::Export(path),
        }] if path == std::path::Path::new("reports/tui.txt")
    ));
}

#[test]
fn platform_commands_render_only_server_reported_truth() {
    let mut state = AppState::new();
    state.connected = true;
    state.platform.features = vec![
        sylvander_protocol::PlatformFeature {
            kind: sylvander_protocol::PlatformFeatureKind::Mcp,
            name: "search".into(),
            status: sylvander_protocol::PlatformFeatureStatus::Configured,
            summary: "configured; runtime health unavailable".into(),
            source: Some("search-mcp".into()),
            trust: Some(sylvander_protocol::PlatformTrust::External),
            auth: sylvander_protocol::PlatformAuthStatus::Configured,
            capabilities: Vec::new(),
            reloadable: false,
        },
        sylvander_protocol::PlatformFeature {
            kind: sylvander_protocol::PlatformFeatureKind::Memory,
            name: "runtime memory".into(),
            status: sylvander_protocol::PlatformFeatureStatus::Active,
            summary: "long-term memory is available".into(),
            source: Some("runtime injection".into()),
            trust: Some(sylvander_protocol::PlatformTrust::BuiltIn),
            auth: sylvander_protocol::PlatformAuthStatus::NotRequired,
            capabilities: vec!["search".into()],
            reloadable: false,
        },
        sylvander_protocol::PlatformFeature {
            kind: sylvander_protocol::PlatformFeatureKind::Hook,
            name: "lint".into(),
            status: sylvander_protocol::PlatformFeatureStatus::Configured,
            summary: "before-tool · blocking".into(),
            source: None,
            trust: Some(sylvander_protocol::PlatformTrust::User),
            auth: sylvander_protocol::PlatformAuthStatus::NotRequired,
            capabilities: vec!["before_tool".into()],
            reloadable: false,
        },
        sylvander_protocol::PlatformFeature {
            kind: sylvander_protocol::PlatformFeatureKind::Extension,
            name: "agent configuration".into(),
            status: sylvander_protocol::PlatformFeatureStatus::Active,
            summary: "1 tools · 1 commands · 1 presentations".into(),
            source: Some("agent definition".into()),
            trust: Some(sylvander_protocol::PlatformTrust::Workspace),
            auth: sylvander_protocol::PlatformAuthStatus::NotRequired,
            capabilities: vec!["tool_presentations".into()],
            reloadable: false,
        },
    ];

    execute(parse("/mcp").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(report))
            if report.contains("configured · auth configured · trust external · reload no")
                && report.contains("source search-mcp")
    ));
    execute(parse("/memory").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(report))
            if report.contains("active · auth not-required · trust built-in · reload no")
                && report.contains("capabilities search")
    ));
    execute(parse("/skills").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(report)) if report.contains("No Skills advertised")
    ));
    execute(parse("/hooks").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(report))
            if report.contains("lint")
                && report.contains("before-tool · blocking")
                && report.contains("capabilities before_tool")
    ));
    execute(parse("/extensions").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::Info(report))
            if report.contains("1 presentations")
                && report.contains("capabilities tool_presentations")
    ));
}

fn dynamic_command(
    name: &str,
    trust: sylvander_protocol::PlatformTrust,
) -> sylvander_protocol::UiCommandDescriptor {
    sylvander_protocol::UiCommandDescriptor {
        id: format!("workspace.{name}"),
        name: name.into(),
        usage: format!("/{name} [scope]"),
        description: "Review a workspace scope".into(),
        hint: "workspace command".into(),
        source: "agent configuration".into(),
        trust,
        effect: sylvander_protocol::UiCommandEffect::SubmitPrompt {
            template: "Review {{args}} for security issues.".into(),
        },
    }
}

#[test]
fn trusted_dynamic_command_submits_through_the_normal_chat_path() {
    let mut state = AppState::new();
    state.connected = true;
    state.session_id = Some("session-1".into());
    state.platform.commands = vec![dynamic_command(
        "security-review",
        sylvander_protocol::PlatformTrust::Workspace,
    )];

    execute_line("/security-review src/auth", &mut state).unwrap();

    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::SendChat { text, .. }]
            if text == "Review src/auth for security issues."
    ));
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::User(text)) if text == "Review src/auth for security issues."
    ));

    state.pending_actions.clear();
    execute_line("/security-review src/session", &mut state).unwrap();
    assert!(state.pending_actions.is_empty());
    assert_eq!(
        state.queued_prompts.front().map(String::as_str),
        Some("Review src/session for security issues.")
    );
    assert!(matches!(
        state.messages.last(),
        Some(ChatMessage::QueuedUser(text))
            if text == "Review src/session for security issues."
    ));
}

#[test]
fn dynamic_registry_exposes_collision_duplicate_and_trust_failures() {
    let mut state = AppState::new();
    state.connected = true;
    state.platform.commands = vec![
        dynamic_command("status", sylvander_protocol::PlatformTrust::Workspace),
        dynamic_command(
            "review-security",
            sylvander_protocol::PlatformTrust::External,
        ),
        dynamic_command(
            "review-security",
            sylvander_protocol::PlatformTrust::Workspace,
        ),
    ];
    state.platform.commands[2].id = state.platform.commands[1].id.clone();

    let matches = ranked_commands("", &state);
    let reasons = matches
        .iter()
        .filter(|entry| entry.dynamic)
        .filter_map(|entry| entry.availability.reason())
        .collect::<Vec<_>>();
    assert!(reasons.iter().any(|reason| reason.contains("built-in")));
    assert!(reasons.iter().any(|reason| reason.contains("not trusted")));
    assert!(reasons.iter().any(|reason| reason.contains("duplicates")));
    assert!(execute_line("/review-security", &mut state).is_err());
}

#[test]
fn model_command_uses_only_server_advertised_combinations() {
    let mut state = AppState::new();
    state.connected = true;
    state.session_id = Some("session-1".into());
    state.metadata.models = vec![sylvander_protocol::ModelDescriptor {
        id: "thinking".into(),
        provider: "test".into(),
        capabilities: 0,
        capability_names: Vec::new(),
        reasoning_efforts: vec![
            sylvander_protocol::ReasoningEffort::Off,
            sylvander_protocol::ReasoningEffort::Medium,
        ],
        lifecycle: sylvander_protocol::ModelLifecycle::Active,
        pricing: None,
    }];
    execute(parse("model thinking medium").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::SelectModel {
            session_id,
            model,
            reasoning_effort: sylvander_protocol::ReasoningEffort::Medium,
        }] if session_id == "session-1"
            && model.provider_id == "test"
            && model.model_id == "thinking"
    ));
    assert!(execute(parse("model thinking high").unwrap(), &mut state).is_err());
    assert!(execute(parse("model missing off").unwrap(), &mut state).is_err());
}

#[test]
fn model_command_requires_provider_for_shared_ids() {
    let mut state = AppState::new();
    state.connected = true;
    state.session_id = Some("session-1".into());
    state.metadata.models = ["alpha", "beta"]
        .into_iter()
        .map(|provider| sylvander_protocol::ModelDescriptor {
            id: "shared".into(),
            provider: provider.into(),
            capabilities: 0,
            capability_names: Vec::new(),
            reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
            lifecycle: sylvander_protocol::ModelLifecycle::Active,
            pricing: None,
        })
        .collect();

    assert!(execute(parse("model shared").unwrap(), &mut state).is_err());
    execute(parse("model beta/shared").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::SelectModel { model, .. }]
            if model.provider_id == "beta" && model.model_id == "shared"
    ));
}

#[test]
fn inspect_and_copy_resolve_completed_tool_outputs_by_prefix() {
    let mut state = AppState::new();
    state.messages.push(ChatMessage::ToolStep {
        name: "Run".into(),
        started_at_secs: 0,
        children: vec![crate::app::ToolStepChild {
            call_id: "call-abcdef".into(),
            name: "bash".into(),
            status: crate::app::ToolStatus::Done,
            input: serde_json::json!({"command":"test"}),
            output: Some("line one\nline two".into()),
            is_error: Some(false),
        }],
    });
    execute(parse("inspect call-a").unwrap(), &mut state).unwrap();
    assert_eq!(
        state.modals.top().map(crate::modal::Modal::title),
        Some("Tool output")
    );
    state.modals.pop();
    execute(parse("attachments tool call-a").unwrap(), &mut state).unwrap();
    assert_eq!(
        state.composer.attachments[0].kind,
        AttachmentKind::TerminalOutput
    );
    execute(parse("copy call-a").unwrap(), &mut state).unwrap();
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::CopyText { text }] if text == "line one\nline two"
    ));
}

#[test]
fn queue_commands_edit_and_remove_waiting_prompts() {
    let mut state = AppState::new();
    state.queued_prompts.push_back("first".into());
    state.messages.push(ChatMessage::QueuedUser("first".into()));

    execute(parse("queue edit 1 updated prompt").unwrap(), &mut state).unwrap();
    assert_eq!(state.queued_prompts[0], "updated prompt");
    assert!(matches!(
        state.messages[0],
        ChatMessage::QueuedUser(ref text) if text == "updated prompt"
    ));

    execute(parse("queue drop 1").unwrap(), &mut state).unwrap();
    assert!(state.queued_prompts.is_empty());
    assert!(state.messages.is_empty());
}

#[test]
fn tasks_cancel_requires_one_running_task_and_keeps_session_scope() {
    let mut state = AppState::new();
    state.session_id = Some("session-1".into());
    state.messages.push(ChatMessage::TaskList {
        tasks: vec![crate::app::TaskEntry {
            task_id: "abcdef12-3456".into(),
            owner: "sylvander".into(),
            purpose: "Inspect".into(),
            state: crate::app::TaskState::Running,
            detail: "iteration 1".into(),
        }],
    });

    execute(parse("tasks cancel abcdef12").unwrap(), &mut state).unwrap();
    assert!(matches!(
        &state.pending_actions[0],
        crate::event::Action::CancelTask { session_id, task_id }
            if session_id == "session-1" && task_id == "abcdef12-3456"
    ));
}

#[test]
fn attachments_commands_reorder_and_remove_draft_context() {
    let mut state = AppState::new();
    state
        .composer
        .attachments
        .push(crate::input::Attachment::new_paste("first".into()));
    state
        .composer
        .attachments
        .push(crate::input::Attachment::new_paste("second".into()));
    execute(parse("attachments up 2").unwrap(), &mut state).unwrap();
    assert_eq!(state.composer.attachments[0].content, "second");
    execute(parse("attachments drop 1").unwrap(), &mut state).unwrap();
    assert_eq!(state.composer.attachments.len(), 1);
    assert_eq!(state.composer.attachments[0].content, "first");
}

#[test]
fn preview_requires_host_and_preserves_session_scope() {
    let mut state = AppState::new();
    state.session_id = Some("session-preview".into());
    let spec = resolve("preview").unwrap();
    assert_eq!(
        availability(spec, &state).reason(),
        Some("requires a trusted desktop host")
    );

    state.host_preview_available = true;
    execute(
        parse("preview image artifacts/result.png").unwrap(),
        &mut state,
    )
    .unwrap();
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::HostPreview {
            session_id,
            kind: crate::host_bridge::PreviewKind::Image,
            target,
        }] if session_id == "session-preview" && target == "artifacts/result.png"
    ));
}
