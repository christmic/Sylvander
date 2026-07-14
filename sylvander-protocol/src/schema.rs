//! Generated, transport-neutral JSON Schema for UI clients.

use serde_json::{Value, json};

use crate::{
    AgentAdminRequest, AgentAdminResponse, UI_PROTOCOL_MAX_VERSION, UI_PROTOCOL_MIN_VERSION,
    UiClientMessage, UiServerMessage,
};

#[must_use]
pub fn agent_admin_protocol_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Sylvander Agent Administration Protocol",
        "request": schemars::schema_for!(AgentAdminRequest),
        "response": schemars::schema_for!(AgentAdminResponse)
    })
}

#[must_use]
pub fn ui_protocol_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Sylvander UI Protocol",
        "protocol": {
            "min_version": UI_PROTOCOL_MIN_VERSION,
            "max_version": UI_PROTOCOL_MAX_VERSION
        },
        "client_message": schemars::schema_for!(UiClientMessage),
        "server_message": schemars::schema_for!(UiServerMessage),
        "agent_administration": agent_admin_protocol_schema()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_v2_schema_keeps_v1_and_new_operations_visible() {
        let schema = ui_protocol_schema();
        assert_eq!(schema["protocol"]["min_version"], 1);
        assert_eq!(schema["protocol"]["max_version"], 2);
        let encoded = serde_json::to_string(&schema).unwrap();
        for operation in [
            "chat",
            "approve",
            "list_sessions",
            "discover_agents",
            "create_session",
            "update_session_config",
            "submit_feedback",
            "agent_admin",
        ] {
            assert!(encoded.contains(operation), "schema omitted {operation}");
        }
    }

    #[test]
    fn agent_administration_schema_exposes_lifecycle_without_secret_values() {
        let encoded = serde_json::to_string(&agent_admin_protocol_schema()).unwrap();
        for operation in [
            "inspect_revision",
            "list_revisions",
            "update_definition",
            "activate_revision",
            "rollback_revision",
        ] {
            assert!(encoded.contains(operation), "schema omitted {operation}");
        }
        assert!(encoded.contains("AgentSecretReference"));
        assert!(!encoded.contains("secret_value"));
        assert!(!encoded.contains("api_key"));
    }
}
