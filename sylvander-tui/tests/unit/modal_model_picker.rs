use super::*;

fn state() -> AppState {
    let mut state = AppState::new();
    state.metadata.model = "plain".into();
    state.metadata.models = vec![
        sylvander_protocol::ModelDescriptor {
            id: "plain".into(),
            provider: "test".into(),
            capabilities: 0,
            capability_names: Vec::new(),
            reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
            lifecycle: sylvander_protocol::ModelLifecycle::Active,
            pricing: None,
        },
        sylvander_protocol::ModelDescriptor {
            id: "thinking".into(),
            provider: "test".into(),
            capabilities: 0,
            capability_names: Vec::new(),
            reasoning_efforts: vec![
                sylvander_protocol::ReasoningEffort::Off,
                sylvander_protocol::ReasoningEffort::Low,
            ],
            lifecycle: sylvander_protocol::ModelLifecycle::Deprecated {
                replacement: Some("plain".into()),
            },
            pricing: None,
        },
    ];
    state
}

#[test]
fn keyboard_selects_only_server_advertised_effort() {
    let mut state = state();
    state.session_id = Some("session-1".into());
    let mut picker = ModelPicker::new(&state);
    picker.handle_key(&KeyEvent::from(KeyCode::Down), &mut state);
    picker.handle_key(&KeyEvent::from(KeyCode::Right), &mut state);
    assert_eq!(
        picker.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state),
        Consumed::Yes { dismiss: true }
    );
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::SelectModel {
            session_id,
            model,
            reasoning_effort: sylvander_protocol::ReasoningEffort::Low,
        }] if session_id == "session-1"
            && model.provider_id == "test"
            && model.model_id == "thinking"
    ));
}

#[test]
fn keyboard_keeps_provider_when_model_ids_are_shared() {
    let mut state = state();
    state.session_id = Some("session-1".into());
    state.metadata.models = vec![
        sylvander_protocol::ModelDescriptor {
            id: "shared".into(),
            provider: "alpha".into(),
            capabilities: 0,
            capability_names: Vec::new(),
            reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
            lifecycle: sylvander_protocol::ModelLifecycle::Active,
            pricing: None,
        },
        sylvander_protocol::ModelDescriptor {
            id: "shared".into(),
            provider: "beta".into(),
            capabilities: 0,
            capability_names: Vec::new(),
            reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
            lifecycle: sylvander_protocol::ModelLifecycle::Active,
            pricing: None,
        },
    ];
    let mut picker = ModelPicker::new(&state);
    picker.handle_key(&KeyEvent::from(KeyCode::Down), &mut state);
    picker.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state);

    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::SelectModel { model, .. }]
            if model.provider_id == "beta" && model.model_id == "shared"
    ));
}
