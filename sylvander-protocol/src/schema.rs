//! Generated, transport-neutral JSON Schema for UI clients.

use serde_json::{Value, json};

use crate::{
    AgentAdminRequest, AgentAdminResponse, IdentityBindingRequest, IdentityBindingResponse,
    RegistryAdminRequest, RegistryAdminResponse, UI_PROTOCOL_MAX_VERSION, UI_PROTOCOL_MIN_VERSION,
    UiClientMessage, UiServerMessage, UserProfileRequest, UserProfileResponse,
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
pub fn registry_admin_protocol_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Sylvander Registry Administration Protocol",
        "request": schemars::schema_for!(RegistryAdminRequest),
        "response": schemars::schema_for!(RegistryAdminResponse)
    })
}

#[must_use]
pub fn identity_binding_protocol_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Sylvander Identity Binding Protocol",
        "version": crate::IDENTITY_BINDING_PROTOCOL_VERSION,
        "request": schemars::schema_for!(IdentityBindingRequest),
        "response": schemars::schema_for!(IdentityBindingResponse)
    })
}

#[must_use]
pub fn user_profile_protocol_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Sylvander User Profile Protocol",
        "version": crate::USER_PROFILE_PROTOCOL_VERSION,
        "request": schemars::schema_for!(UserProfileRequest),
        "response": schemars::schema_for!(UserProfileResponse)
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
        "identity_binding": identity_binding_protocol_schema(),
        "user_profile": user_profile_protocol_schema(),
        "agent_administration": agent_admin_protocol_schema(),
        "registry_administration": registry_admin_protocol_schema()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_v3_schema_keeps_legacy_and_current_operations_visible() {
        let schema = ui_protocol_schema();
        assert_eq!(schema["protocol"]["min_version"], 1);
        assert_eq!(schema["protocol"]["max_version"], 3);
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
            "registry_admin",
            "user_profile",
        ] {
            assert!(encoded.contains(operation), "schema omitted {operation}");
        }
        for capability_contract in ["capability_names", "ModelCapability", "tool_use"] {
            assert!(
                encoded.contains(capability_contract),
                "schema omitted {capability_contract}"
            );
        }
        for prompt_contract in [
            "prompt_manifest",
            "PromptManifest",
            "PromptLayerDigest",
            "shared_safety",
            "provider_model_profile",
            "session_input",
            "aggregate_sha256",
            "total_bytes",
        ] {
            assert!(
                encoded.contains(prompt_contract),
                "schema omitted {prompt_contract}"
            );
        }
    }

    #[test]
    fn session_prompt_is_write_only_in_the_ui_schema() {
        let schema = ui_protocol_schema();
        assert!(
            has_property(&schema["client_message"], "system_prompt"),
            "clients must be able to write a session prompt override"
        );
        assert!(
            !has_property(
                &schema["server_message"]["$defs"]["RedactedSessionConfigOverrides"],
                "system_prompt"
            ),
            "server session override responses must not publish raw prompt input"
        );
        let encoded = serde_json::to_string(&schema["server_message"]).unwrap();
        assert!(encoded.contains("RedactedSessionConfigOverrides"));
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

    #[test]
    fn registry_schema_exposes_registry_reads_without_raw_response_configuration() {
        let schema = registry_admin_protocol_schema();
        let encoded = serde_json::to_string(&schema).unwrap();
        for operation in [
            "inspect_provider_revision",
            "list_provider_revisions",
            "inspect_model_revision",
            "list_model_revisions",
            "inspect_credential_generation",
            "list_credential_generations",
            "create_credential_binding",
            "stage_credential_generation",
            "activate_credential_generation",
            "rollback_credential_generation",
            "base_url_sha256",
            "credential_binding_id_sha256",
            "pricing_sha256",
            "binding_id_sha256",
            "reference_digest_sha256",
            "CredentialReferenceKind",
            "CredentialSecretReferenceDraft",
            "credential_already_exists",
            "active_generation_conflict",
            "non_sequential_generation",
            "generation_collision",
            "invalid_rollback",
            "credential_unavailable",
        ] {
            assert!(encoded.contains(operation), "schema omitted {operation}");
        }
        let response = &schema["response"];
        for field in [
            "base_url",
            "credential_binding_id",
            "pricing",
            "binding_id",
            "reference",
            "path",
            "name",
            "secret_value",
        ] {
            assert!(!has_property(response, field), "response exposed {field}");
        }
    }

    fn has_property(value: &Value, name: &str) -> bool {
        match value {
            Value::Object(object) => {
                object
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|properties| properties.contains_key(name))
                    || object.values().any(|value| has_property(value, name))
            }
            Value::Array(values) => values.iter().any(|value| has_property(value, name)),
            _ => false,
        }
    }
}
