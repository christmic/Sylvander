use super::*;

// -- builder --

#[test]
fn builder_basic() {
    let spec = AgentSpec::builder()
        .id("test-agent")
        .name("Test Agent")
        .system_prompt("You are a test agent.")
        .description("Used for testing")
        .model_name("claude-sonnet-5-20260601")
        .allowed_model("anthropic", "claude-sonnet-5-20260601")
        .allowed_model("secondary", "shared")
        .builtin_tool("read")
        .builtin_tool("write")
        .max_iterations(30)
        .build()
        .expect("build should succeed");

    assert_eq!(spec.id, AgentId::new("test-agent"));
    assert_eq!(spec.name, "Test Agent");
    assert_eq!(spec.persona.system_prompt, "You are a test agent.");
    assert_eq!(spec.persona.description, "Used for testing");
    assert_eq!(spec.model.model_name, "claude-sonnet-5-20260601");
    assert_eq!(
        spec.model.allowed_models,
        [
            ModelSelection {
                provider_id: "anthropic".into(),
                model_id: "claude-sonnet-5-20260601".into(),
            },
            ModelSelection {
                provider_id: "secondary".into(),
                model_id: "shared".into(),
            }
        ]
    );
    assert_eq!(spec.tools.len(), 2);
    assert!(matches!(&spec.tools[0], ToolRef::Builtin { name } if name == "read"));
    assert_eq!(spec.behavior.max_iterations, 30);
    assert_eq!(spec.behavior.max_retries, 3); // default
}

#[test]
fn builder_missing_id() {
    let err = AgentSpec::builder().name("No ID").build().unwrap_err();
    assert!(matches!(err, AgentSpecError::MissingId));
}

#[test]
fn builder_missing_name() {
    let err = AgentSpec::builder().id("no-name").build().unwrap_err();
    assert!(matches!(err, AgentSpecError::MissingName));
}

#[test]
fn builder_defaults() {
    let spec = AgentSpec::builder()
        .id("minimal")
        .name("Minimal Agent")
        .build()
        .expect("build should succeed");

    assert!(spec.persona.system_prompt.is_empty());
    assert!(spec.tools.is_empty());
    assert!(spec.memory_stores.is_empty());
    assert!(spec.ui_commands.is_empty());
    assert_eq!(spec.behavior.max_iterations, 50);
    assert_eq!(spec.behavior.max_retries, 3);
    assert_eq!(spec.model.provider, "anthropic");
    assert!(spec.model.allowed_models.is_empty());
}

// -- TOML --

#[test]
fn toml_roundtrip() {
    let spec = AgentSpec::builder()
        .id("toml-agent")
        .name("TOML Agent")
        .system_prompt("You are defined in TOML.")
        .model_name("claude-haiku-4-5-20251001")
        .allowed_model("anthropic", "claude-haiku-4-5-20251001")
        .allowed_model("secondary", "shared")
        .builtin_tool("read")
        .build()
        .expect("build should succeed");

    let toml_str = toml::to_string_pretty(&spec).expect("serialize");
    let parsed: AgentSpec = toml::from_str(&toml_str).expect("deserialize");

    assert_eq!(parsed.id, spec.id);
    assert_eq!(parsed.name, spec.name);
    assert_eq!(parsed.persona.system_prompt, spec.persona.system_prompt);
    assert_eq!(parsed.model.model_name, spec.model.model_name);
    assert_eq!(parsed.model.allowed_models, spec.model.allowed_models);
    assert_eq!(parsed.tools.len(), spec.tools.len());
}

#[test]
fn prompt_command_round_trips_through_toml() {
    let spec = AgentSpec::builder()
        .id("commands")
        .name("Commands")
        .ui_command(UiCommandConfig {
            id: "review-security".into(),
            name: "security-review".into(),
            usage: "/security-review [scope]".into(),
            description: "Review a scope".into(),
            hint: "workspace".into(),
            prompt: "Review {{args}} for security issues.".into(),
        })
        .build()
        .unwrap();

    let encoded = toml::to_string_pretty(&spec).unwrap();
    let restored: AgentSpec = toml::from_str(&encoded).unwrap();
    assert_eq!(restored.ui_commands.len(), 1);
    assert_eq!(restored.ui_commands[0].name, "security-review");
}

#[test]
fn toml_deserialize_minimal() {
    let toml_str = r#"
id = "minimal-toml"
name = "Minimal TOML Agent"
"#;
    let spec: AgentSpec = toml::from_str(toml_str).expect("deserialize");
    assert_eq!(spec.id, AgentId::new("minimal-toml"));
    assert_eq!(spec.name, "Minimal TOML Agent");
    assert!(spec.persona.system_prompt.is_empty());
}

#[test]
fn toml_rejects_removed_parallel_mcp_registry() {
    let toml_str = r#"
id = "legacy-mcp"
name = "Legacy MCP"

[[mcp_servers]]
name = "search"
command = "search-mcp"
"#;

    let error = toml::from_str::<AgentSpec>(toml_str).unwrap_err();
    assert!(error.to_string().contains("unknown field `mcp_servers`"));
}

#[test]
fn toml_deserialize_full() {
    let toml_str = r#"
id = "full-agent"
name = "Full Agent"

[persona]
system_prompt = "You are a helpful assistant."
description = "A fully configured agent"

[model]
provider = "anthropic"
model_name = "claude-sonnet-5-20260601"
allowed_models = [
  { provider_id = "anthropic", model_id = "claude-sonnet-5-20260601" },
]
temperature = 0.7
max_tokens = 4096

[[tools]]
type = "builtin"
name = "read"

[[tools]]
type = "builtin"
name = "write"

[[tools]]
type = "mcp_server"
name = "code-analyzer"
command = "code-analyzer-mcp"
args = ["--verbose"]

[[memory_stores]]
store_type = "sqlite"
path = "/tmp/agent-memory.db"

[behavior]
max_iterations = 30
max_retries = 5
"#;
    let spec: AgentSpec = toml::from_str(toml_str).expect("deserialize");

    assert_eq!(spec.id, AgentId::new("full-agent"));
    assert_eq!(spec.persona.system_prompt, "You are a helpful assistant.");
    assert_eq!(spec.model.temperature, Some(0.7));
    assert_eq!(spec.model.max_tokens, Some(4096));
    assert_eq!(spec.model.allowed_models.len(), 1);
    assert_eq!(spec.tools.len(), 3);
    assert_eq!(spec.memory_stores.len(), 1);
    assert_eq!(spec.memory_stores[0].store_type, "sqlite");
    assert_eq!(spec.behavior.max_iterations, 30);
    assert_eq!(spec.behavior.max_retries, 5);
}

#[test]
fn toml_model_requires_explicit_allowed_models() {
    let error = toml::from_str::<AgentSpec>(
        r#"
id = "missing-allowlist"
name = "Missing Allowlist"

[model]
provider = "anthropic"
model_name = "claude-sonnet-5-20260601"
"#,
    )
    .unwrap_err();

    assert!(error.to_string().contains("allowed_models"));
}

// -- ModelInfo conversion --

#[test]
fn to_model_info() {
    let spec = AgentSpec::builder()
        .id("model-test")
        .name("Model Test")
        .model(ModelConfig {
            provider: "anthropic".into(),
            model_name: "claude-sonnet-5-20260601".into(),
            allowed_models: vec![ModelSelection {
                provider_id: "anthropic".into(),
                model_id: "claude-sonnet-5-20260601".into(),
            }],
            temperature: Some(0.5),
            max_tokens: Some(8192),
        })
        .build()
        .expect("build should succeed");

    let info = spec.to_model_info().unwrap();
    assert_eq!(info.id, "claude-sonnet-5-20260601");
    assert_eq!(info.max_output_tokens, 8192);
    assert_eq!(info.context_window, 200_000);
}

#[test]
fn to_model_info_default_max_tokens() {
    let spec = AgentSpec::builder()
        .id("default-tokens")
        .name("Default Tokens")
        .model_name("claude-opus-4-8")
        .build()
        .expect("build should succeed");

    let info = spec.to_model_info().unwrap();
    assert_eq!(info.max_output_tokens, 32_000);
}

#[test]
fn to_model_info_rejects_invalid_declarative_input_without_panicking() {
    let mut spec = AgentSpec::builder()
        .id("invalid-model")
        .name("Invalid Model")
        .build()
        .unwrap();
    spec.model.model_name.clear();

    assert!(matches!(
        spec.to_model_info(),
        Err(AgentSpecError::InvalidModel)
    ));
}

// -- ID types --

#[test]
fn agent_id_display() {
    let id = AgentId::new("test-123");
    assert_eq!(format!("{id}"), "test-123");
}

#[test]
fn agent_id_from_str() {
    let id: AgentId = "from-str".into();
    assert_eq!(id.0, "from-str");
}

#[test]
fn session_id_display() {
    let id = SessionId::new("session-456");
    assert_eq!(format!("{id}"), "session-456");
}

// -- BehaviorConfig defaults --

#[test]
fn behavior_config_default() {
    let b = BehaviorConfig::default();
    assert_eq!(b.max_iterations, 50);
    assert_eq!(b.max_retries, 3);
}
