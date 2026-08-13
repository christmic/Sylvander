//! Narrow native-host services that do not belong to Runtime or React.
//!
//! Host preferences are local presentation state. They never enter a Session,
//! Runtime command, transcript, diagnostic, or user-profile record.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{App, AppHandle, Manager, State, Wry};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_store::{Store, StoreExt};

use sylvander_api::UiServerMessage;

const PREFERENCES_FILE: &str = "desktop-preferences.json";
const TURN_NOTIFICATIONS_KEY: &str = "turn_notifications";

pub(crate) struct DesktopHost {
    turn_notifications: AtomicBool,
    store: std::sync::Arc<Store<Wry>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct HostPreferences {
    turn_notifications: bool,
}

pub(crate) fn setup(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let store = app
        .store_builder(PREFERENCES_FILE)
        .default(TURN_NOTIFICATIONS_KEY, false)
        .disable_auto_save()
        .build()?;
    let enabled = store
        .get(TURN_NOTIFICATIONS_KEY)
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    app.manage(DesktopHost {
        turn_notifications: AtomicBool::new(enabled),
        store,
    });
    Ok(())
}

#[tauri::command]
pub(crate) fn get_host_preferences(host: State<'_, DesktopHost>) -> HostPreferences {
    HostPreferences {
        turn_notifications: host.turn_notifications.load(Ordering::Acquire),
    }
}

#[tauri::command]
pub(crate) fn set_turn_notifications(
    enabled: bool,
    host: State<'_, DesktopHost>,
) -> Result<HostPreferences, String> {
    host.store.set(TURN_NOTIFICATIONS_KEY, enabled);
    host.store
        .save()
        .map_err(|_| "Desktop preferences could not be saved".to_owned())?;
    host.turn_notifications.store(enabled, Ordering::Release);
    Ok(HostPreferences {
        turn_notifications: enabled,
    })
}

pub(crate) fn notify_if_backgrounded(app: &AppHandle, body: &'static str) {
    let host = app.state::<DesktopHost>();
    if !host.turn_notifications.load(Ordering::Acquire) || main_window_is_focused(app) {
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title("Sylvander Work")
        .body(body)
        .show();
}

fn main_window_is_focused(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_focused().ok())
        .unwrap_or(true)
}

pub(crate) fn terminal_notification_body(message: &UiServerMessage) -> Option<&'static str> {
    match message {
        UiServerMessage::Done { .. } => Some("Agent turn completed"),
        UiServerMessage::Error { .. } => Some("Agent turn failed"),
        UiServerMessage::TurnInterrupted { .. } => Some("Agent turn interrupted"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_runtime_turn_terminals_create_content_free_notifications() {
        let done = super::UiServerMessage::Done {
            session_id: "session-1".into(),
            text: "private answer".into(),
            feedback_target: None,
        };
        let failed = super::UiServerMessage::Error {
            session_id: "session-1".into(),
            message: "private failure".into(),
            feedback_target: None,
        };
        let interrupted = super::UiServerMessage::TurnInterrupted {
            session_id: "session-1".into(),
            reason: "private reason".into(),
            feedback_target: None,
        };
        let delta = super::UiServerMessage::TextDelta {
            session_id: "session-1".into(),
            delta: "private delta".into(),
        };

        assert_eq!(
            super::terminal_notification_body(&done),
            Some("Agent turn completed")
        );
        assert_eq!(
            super::terminal_notification_body(&failed),
            Some("Agent turn failed")
        );
        assert_eq!(
            super::terminal_notification_body(&interrupted),
            Some("Agent turn interrupted")
        );
        assert_eq!(super::terminal_notification_body(&delta), None);
    }
}
