//! Typed editor and deletion confirmation for the owner-scoped User Profile.
//!
//! The editor never asks users to author protocol JSON. It builds one complete,
//! validated replacement payload and binds mutations to the revision returned
//! by Runtime.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};
use sylvander_protocol::{
    AccessibilityPreferences, ClassifiedPreference, CommunicationTone, LanguageTag, LocaleId,
    PrivacyClass, ProfileConstraint, ResponseDetail, USER_PROFILE_PROTOCOL_VERSION,
    UserProfileAction, UserProfileData, UserProfileRequest, UserProfileView,
};

use crate::app::AppState;
use crate::modal::{Consumed, Modal, ModalPlacement, surface::decision_dock};
use crate::theme;

const FIELD_COUNT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileEditMode {
    Create,
    Update,
    Correct,
}

pub struct ProfileEditor {
    mode: ProfileEditMode,
    revision: Option<u64>,
    selected: usize,
    editing: bool,
    edit_buffer: String,
    language: String,
    locale: String,
    detail: Option<ResponseDetail>,
    tone: Option<CommunicationTone>,
    accessibility: AccessibilityPreferences,
    constraints: Vec<String>,
    constraint_index: usize,
}

impl ProfileEditor {
    pub fn new(mode: ProfileEditMode, current: Option<&UserProfileView>) -> Self {
        let profile = current.map(|view| &view.profile);
        let accessibility = profile
            .and_then(|profile| profile.accessibility.as_ref())
            .map_or_else(
                || AccessibilityPreferences {
                    screen_reader_optimized: false,
                    reduce_motion: false,
                    high_contrast: false,
                },
                |value| value.value.clone(),
            );
        let constraints = profile.map_or_else(Vec::new, |profile| {
            profile
                .constraints
                .iter()
                .map(|value| value.value.as_str().to_owned())
                .collect()
        });
        Self {
            mode,
            revision: current.map(|view| view.revision),
            selected: 0,
            editing: false,
            edit_buffer: String::new(),
            language: profile
                .and_then(|profile| profile.preferred_language.as_ref())
                .map_or_else(String::new, |value| value.value.as_str().to_owned()),
            locale: profile
                .and_then(|profile| profile.locale.as_ref())
                .map_or_else(String::new, |value| value.value.as_str().to_owned()),
            detail: profile
                .and_then(|profile| profile.response_detail.as_ref())
                .map(|value| value.value),
            tone: profile
                .and_then(|profile| profile.communication_tone.as_ref())
                .map(|value| value.value),
            accessibility,
            constraints,
            constraint_index: 0,
        }
    }

    fn begin_edit(&mut self) {
        self.edit_buffer = match self.selected {
            0 => self.language.clone(),
            1 => self.locale.clone(),
            7 => self
                .constraints
                .get(self.constraint_index)
                .cloned()
                .unwrap_or_default(),
            _ => return,
        };
        self.editing = true;
    }

    fn commit_edit(&mut self) {
        let value = self.edit_buffer.trim().to_owned();
        match self.selected {
            0 => self.language = value,
            1 => self.locale = value,
            7 if self.constraint_index < self.constraints.len() => {
                if value.is_empty() {
                    self.constraints.remove(self.constraint_index);
                    self.constraint_index = self
                        .constraint_index
                        .min(self.constraints.len().saturating_sub(1));
                } else {
                    self.constraints[self.constraint_index] = value;
                }
            }
            7 if !value.is_empty() && self.constraints.len() < 16 => {
                self.constraints.push(value);
                self.constraint_index = self.constraints.len() - 1;
            }
            _ => {}
        }
        self.editing = false;
        self.edit_buffer.clear();
    }

    fn cycle_selected(&mut self, forward: bool) {
        match self.selected {
            2 => {
                const VALUES: [Option<ResponseDetail>; 4] = [
                    None,
                    Some(ResponseDetail::Concise),
                    Some(ResponseDetail::Balanced),
                    Some(ResponseDetail::Detailed),
                ];
                self.detail = next(&VALUES, self.detail, forward);
            }
            3 => {
                const VALUES: [Option<CommunicationTone>; 4] = [
                    None,
                    Some(CommunicationTone::Direct),
                    Some(CommunicationTone::Warm),
                    Some(CommunicationTone::Formal),
                ];
                self.tone = next(&VALUES, self.tone, forward);
            }
            4 => {
                self.accessibility.screen_reader_optimized =
                    !self.accessibility.screen_reader_optimized;
            }
            5 => self.accessibility.reduce_motion = !self.accessibility.reduce_motion,
            6 => self.accessibility.high_contrast = !self.accessibility.high_contrast,
            7 if !self.constraints.is_empty() => {
                self.constraint_index = if forward {
                    (self.constraint_index + 1) % self.constraints.len()
                } else {
                    (self.constraint_index + self.constraints.len() - 1) % self.constraints.len()
                };
            }
            _ => {}
        }
    }

    fn request(&self) -> Result<UserProfileRequest, String> {
        let preferred_language = if self.language.is_empty() {
            None
        } else {
            Some(personal(
                LanguageTag::new(self.language.clone())
                    .map_err(|error| format!("Preferred language: {error}"))?,
            ))
        };
        let locale = if self.locale.is_empty() {
            None
        } else {
            Some(personal(
                LocaleId::new(self.locale.clone()).map_err(|error| format!("Locale: {error}"))?,
            ))
        };
        let has_accessibility = self.accessibility.screen_reader_optimized
            || self.accessibility.reduce_motion
            || self.accessibility.high_contrast;
        let constraints = self
            .constraints
            .iter()
            .map(|value| {
                ProfileConstraint::new(value.clone())
                    .map(|value| ClassifiedPreference {
                        value,
                        privacy_class: PrivacyClass::Sensitive,
                    })
                    .map_err(|error| format!("Constraint: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let profile = UserProfileData {
            preferred_language,
            locale,
            response_detail: self.detail.map(personal),
            communication_tone: self.tone.map(personal),
            accessibility: has_accessibility.then(|| personal(self.accessibility.clone())),
            constraints,
        };
        let action = match self.mode {
            ProfileEditMode::Create => UserProfileAction::Create { profile },
            ProfileEditMode::Update => UserProfileAction::Update {
                expected_revision: self.revision.ok_or_else(|| {
                    "Profile revision is unavailable; reload it first".to_string()
                })?,
                profile,
            },
            ProfileEditMode::Correct => UserProfileAction::Correct {
                expected_revision: self.revision.ok_or_else(|| {
                    "Profile revision is unavailable; reload it first".to_string()
                })?,
                profile,
            },
        };
        Ok(UserProfileRequest {
            version: USER_PROFILE_PROTOCOL_VERSION,
            action,
        })
    }

    fn save(&self, state: &mut AppState) -> Consumed {
        match self.request() {
            Ok(request) => {
                state
                    .pending_actions
                    .push(crate::event::Action::UserProfile { request });
                state.status = match self.mode {
                    ProfileEditMode::Create => "Creating user profile…",
                    ProfileEditMode::Update => "Updating user profile…",
                    ProfileEditMode::Correct => "Correcting user profile…",
                }
                .into();
                Consumed::Yes { dismiss: true }
            }
            Err(error) => {
                state.status = error;
                Consumed::Yes { dismiss: false }
            }
        }
    }
}

impl Modal for ProfileEditor {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "User profile"
    }

    fn placement(&self, _state: &AppState, _viewport_width: u16) -> ModalPlacement {
        ModalPlacement::BelowComposer { rows: 13 }
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let body = decision_dock(frame, parent, 12);
        let mode = match self.mode {
            ProfileEditMode::Create => "create",
            ProfileEditMode::Update => "update",
            ProfileEditMode::Correct => "correct",
        };
        let mut lines = vec![Line::from(vec![
            Span::styled("User profile", theme::brand_violet()),
            Span::styled(
                format!(
                    " · {mode}{} · server revision",
                    self.revision
                        .map_or_else(String::new, |value| format!(" r{value}"))
                ),
                theme::text_muted(),
            ),
        ])];
        let values = [
            if self.editing && self.selected == 0 {
                format!("{}▌", self.edit_buffer)
            } else {
                optional_text(&self.language)
            },
            if self.editing && self.selected == 1 {
                format!("{}▌", self.edit_buffer)
            } else {
                optional_text(&self.locale)
            },
            detail_label(self.detail).into(),
            tone_label(self.tone).into(),
            bool_label(self.accessibility.screen_reader_optimized).into(),
            bool_label(self.accessibility.reduce_motion).into(),
            bool_label(self.accessibility.high_contrast).into(),
            constraint_label(self),
        ];
        let labels = [
            "language",
            "locale",
            "detail",
            "tone",
            "screen reader",
            "reduce motion",
            "high contrast",
            "constraints",
        ];
        for (index, (label, value)) in labels.into_iter().zip(values).enumerate() {
            let selected = index == self.selected;
            lines.push(Line::from(vec![
                Span::styled(if selected { "› " } else { "  " }, theme::active_bold()),
                Span::styled(format!("{label:<15}"), theme::text_muted()),
                Span::styled(
                    value,
                    if selected {
                        theme::active_bold()
                    } else {
                        theme::text_dim()
                    },
                ),
            ]));
        }
        lines.push(Line::from(Span::styled(
            if self.editing {
                "enter apply field · esc discard field"
            } else {
                "↑↓ field · enter edit/toggle · ←→ choose · a/d constraint · s save · esc cancel"
            },
            theme::text_muted(),
        )));
        frame.render_widget(Paragraph::new(lines), body);
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        if self.editing {
            match key.code {
                KeyCode::Enter => self.commit_edit(),
                KeyCode::Esc => {
                    self.editing = false;
                    self.edit_buffer.clear();
                }
                KeyCode::Backspace => {
                    self.edit_buffer.pop();
                }
                KeyCode::Char(character)
                    if !character.is_control()
                        && self.edit_buffer.len() + character.len_utf8() <= 512 =>
                {
                    self.edit_buffer.push(character);
                }
                _ => {}
            }
            state.dirty.mark();
            return Consumed::Yes { dismiss: false };
        }
        match key.code {
            KeyCode::Esc => {
                state.status = "User profile edit cancelled".into();
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(FIELD_COUNT - 1);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Left | KeyCode::Right => {
                self.cycle_selected(key.code == KeyCode::Right);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter if matches!(self.selected, 0 | 1 | 7) => {
                self.begin_edit();
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter => {
                self.cycle_selected(true);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('a') if self.selected == 7 && self.constraints.len() < 16 => {
                self.constraint_index = self.constraints.len();
                self.begin_edit();
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('d') if self.selected == 7 && !self.constraints.is_empty() => {
                self.constraints.remove(self.constraint_index);
                self.constraint_index = self
                    .constraint_index
                    .min(self.constraints.len().saturating_sub(1));
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('s') => self.save(state),
            _ => Consumed::Yes { dismiss: false },
        }
    }
}

pub struct ProfileDeleteModal {
    revision: u64,
    selected: usize,
}

impl ProfileDeleteModal {
    pub fn new(revision: u64) -> Self {
        Self {
            revision,
            selected: 0,
        }
    }
}

impl Modal for ProfileDeleteModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Delete user profile"
    }

    fn placement(&self, _state: &AppState, _viewport_width: u16) -> ModalPlacement {
        ModalPlacement::BelowComposer { rows: 7 }
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let body = decision_dock(frame, parent, 6);
        let mut lines = vec![
            Line::from(Span::styled(
                "◆ Delete your stored user profile?",
                theme::danger().bold(),
            )),
            Line::from(Span::styled(
                format!(
                    "Revision r{} is removed. The server may preserve do-not-learn.",
                    self.revision
                ),
                theme::text_muted(),
            )),
            Line::default(),
        ];
        for (index, label) in ["Cancel", "Delete profile"].into_iter().enumerate() {
            lines.push(Line::from(Span::styled(
                format!(
                    "{}{}. {label}",
                    if self.selected == index { "› " } else { "  " },
                    index + 1
                ),
                if self.selected == index && index == 1 {
                    theme::danger().bold()
                } else if self.selected == index {
                    theme::brand_violet().bold()
                } else {
                    theme::text()
                },
            )));
        }
        frame.render_widget(Paragraph::new(lines), body);
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Up | KeyCode::Char('1') => self.selected = 0,
            KeyCode::Down | KeyCode::Char('2') => self.selected = 1,
            KeyCode::Enter if self.selected == 1 => {
                state
                    .pending_actions
                    .push(crate::event::Action::UserProfile {
                        request: UserProfileRequest {
                            version: USER_PROFILE_PROTOCOL_VERSION,
                            action: UserProfileAction::Delete {
                                expected_revision: self.revision,
                            },
                        },
                    });
                state.status = "Deleting user profile…".into();
                return Consumed::Yes { dismiss: true };
            }
            KeyCode::Char('y') => {
                state
                    .pending_actions
                    .push(crate::event::Action::UserProfile {
                        request: UserProfileRequest {
                            version: USER_PROFILE_PROTOCOL_VERSION,
                            action: UserProfileAction::Delete {
                                expected_revision: self.revision,
                            },
                        },
                    });
                state.status = "Deleting user profile…".into();
                return Consumed::Yes { dismiss: true };
            }
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('n') => {
                state.status = "User profile deletion cancelled".into();
                return Consumed::Yes { dismiss: true };
            }
            _ => {}
        }
        state.dirty.mark();
        Consumed::Yes { dismiss: false }
    }
}

fn next<T: Copy + PartialEq>(values: &[T], current: T, forward: bool) -> T {
    let index = values
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    if forward {
        values[(index + 1) % values.len()]
    } else {
        values[(index + values.len() - 1) % values.len()]
    }
}

fn personal<T>(value: T) -> ClassifiedPreference<T> {
    ClassifiedPreference {
        value,
        privacy_class: PrivacyClass::Personal,
    }
}

fn optional_text(value: &str) -> String {
    if value.is_empty() {
        "not set".into()
    } else {
        value.into()
    }
}

fn detail_label(value: Option<ResponseDetail>) -> &'static str {
    match value {
        None => "not set",
        Some(ResponseDetail::Concise) => "concise",
        Some(ResponseDetail::Balanced) => "balanced",
        Some(ResponseDetail::Detailed) => "detailed",
    }
}

fn tone_label(value: Option<CommunicationTone>) -> &'static str {
    match value {
        None => "not set",
        Some(CommunicationTone::Direct) => "direct",
        Some(CommunicationTone::Warm) => "warm",
        Some(CommunicationTone::Formal) => "formal",
    }
}

fn bool_label(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn constraint_label(editor: &ProfileEditor) -> String {
    if editor.editing && editor.selected == 7 {
        return format!("{}▌", editor.edit_buffer);
    }
    editor.constraints.get(editor.constraint_index).map_or_else(
        || "none · a add".into(),
        |value| {
            format!(
                "{}/{} · {}",
                editor.constraint_index + 1,
                editor.constraints.len(),
                value
            )
        },
    )
}

#[cfg(test)]
#[path = "../../tests/unit/modal_profile.rs"]
mod tests;
