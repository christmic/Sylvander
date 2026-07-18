use super::*;

fn agent(id: &str, name: &str) -> sylvander_protocol::AgentDescriptor {
    sylvander_protocol::AgentDescriptor {
        id: sylvander_protocol::AgentId::new(id),
        revision: 1,
        name: name.into(),
        provider_id: "provider".into(),
        default_model_id: "model".into(),
        models: Vec::new(),
        default_prompt_profile: None,
        agent_workspace: None,
    }
}

#[test]
fn switching_agent_leaves_the_old_session_and_starts_fresh() {
    let mut state = AppState::new();
    state.agents = vec![agent("first", "First"), agent("second", "Second")];
    state.selected_agent_id = Some(sylvander_protocol::AgentId::new("first"));
    state.session_id = Some("old-session".into());
    state
        .messages
        .push(crate::app::ChatMessage::User("old".into()));
    let mut picker = AgentPicker::new(&state);

    picker.handle_key(&KeyEvent::from(KeyCode::Down), &mut state);
    assert_eq!(
        picker.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state),
        Consumed::Yes { dismiss: true }
    );
    assert_eq!(
        state.selected_agent_id.as_ref().map(|id| id.0.as_str()),
        Some("second")
    );
    assert!(state.session_id.is_none());
    assert!(state.messages.is_empty());
}
