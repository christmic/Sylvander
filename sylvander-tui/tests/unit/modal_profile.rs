use super::*;

fn current_profile() -> UserProfileView {
    UserProfileView {
        revision: 7,
        profile: UserProfileData::default(),
        do_not_learn: false,
        created_at_unix_secs: 10,
        updated_at_unix_secs: 20,
    }
}

#[test]
fn editor_builds_a_typed_revision_bound_update_without_json_input() {
    let profile = current_profile();
    let mut editor = ProfileEditor::new(ProfileEditMode::Update, Some(&profile));
    editor.language = "zh-Hans".into();
    editor.locale = "zh-CN".into();
    editor.detail = Some(ResponseDetail::Detailed);
    editor.tone = Some(CommunicationTone::Warm);
    editor.accessibility.screen_reader_optimized = true;
    editor.constraints = vec!["Use concise headings".into()];

    let request = editor.request().expect("typed request");
    let UserProfileAction::Update {
        expected_revision,
        profile,
    } = request.action
    else {
        panic!("expected update");
    };
    assert_eq!(expected_revision, 7);
    assert_eq!(
        profile.preferred_language.expect("language").value.as_str(),
        "zh-Hans"
    );
    assert_eq!(profile.locale.expect("locale").value.as_str(), "zh-CN");
    assert!(
        profile
            .accessibility
            .expect("accessibility")
            .value
            .screen_reader_optimized
    );
    assert_eq!(
        profile.constraints[0].value.as_str(),
        "Use concise headings"
    );
}

#[test]
fn stale_update_cannot_be_built_without_a_server_revision() {
    let editor = ProfileEditor::new(ProfileEditMode::Update, None);
    assert!(
        editor
            .request()
            .expect_err("revision must be mandatory")
            .contains("revision")
    );
}

#[test]
fn deleting_a_constraint_while_editing_never_leaves_an_invalid_index() {
    let mut editor = ProfileEditor::new(ProfileEditMode::Create, None);
    editor.selected = 7;
    editor.constraints = vec!["one".into()];
    editor.constraint_index = 0;
    editor.editing = true;
    editor.edit_buffer.clear();
    editor.commit_edit();
    assert!(editor.constraints.is_empty());
    assert_eq!(editor.constraint_index, 0);
}

#[test]
fn profile_delete_is_safe_by_default_and_revision_bound_after_confirmation() {
    let mut state = AppState::new();
    let mut modal = ProfileDeleteModal::new(8);
    assert_eq!(
        modal.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state),
        Consumed::Yes { dismiss: true }
    );
    assert!(state.pending_actions.is_empty());

    let mut modal = ProfileDeleteModal::new(8);
    modal.handle_key(&KeyEvent::from(KeyCode::Down), &mut state);
    modal.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state);
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::UserProfile {
            request: UserProfileRequest {
                action: UserProfileAction::Delete {
                    expected_revision: 8
                },
                ..
            }
        }]
    ));
}
