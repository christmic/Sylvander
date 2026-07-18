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
#[path = "../tests/unit/schema.rs"]
mod tests;
