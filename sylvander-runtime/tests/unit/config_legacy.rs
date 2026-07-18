use super::*;

fn required_values() -> HashMap<String, String> {
    HashMap::from([
        ("SYLVANDER_MODE".into(), "self_use".into()),
        ("SYLVANDER_MODEL".into(), "model-a".into()),
        ("ANTHROPIC_API_KEY".into(), "must-not-be-stored".into()),
        (
            "ANTHROPIC_BASE_URL".into(),
            "https://models.example.test".into(),
        ),
    ])
}

#[test]
fn conversion_keeps_secret_references_not_values() {
    let values = required_values();
    let config = ServerConfig::from_legacy_values(&values).unwrap();
    let encoded = toml::to_string(&config).unwrap();
    assert!(encoded.contains("ANTHROPIC_API_KEY"));
    assert!(!encoded.contains("must-not-be-stored"));
    assert_eq!(config.agents[0].spec.model.provider, "primary");
    assert!(config.agents[0].spec.model.allowed_models.is_empty());
    assert!(config.agents[0].access.allow_authenticated);
    assert!(config.agents[0].access.allowed_principals.is_empty());
}

#[test]
fn conversion_preserves_memory_database_path() {
    let mut values = required_values();
    values.insert(
        "SYLVANDER_MEMORY_DB".into(),
        "/srv/sylvander/memory.db".into(),
    );

    let config = ServerConfig::from_legacy_values(&values).unwrap();

    assert_eq!(
        config.server.memory_db,
        Some(PathBuf::from("/srv/sylvander/memory.db"))
    );
}

#[test]
fn legacy_environment_cannot_override_memory_maintenance_policy() {
    let mut values = required_values();
    values.insert("SYLVANDER_MEMORY_RETENTION_DAYS".into(), "9999".into());
    values.insert("SYLVANDER_MEMORY_BATCH_SIZE".into(), "0".into());

    let config = ServerConfig::from_legacy_values(&values).unwrap();

    assert_eq!(
        config.server.memory_maintenance,
        super::super::MemoryMaintenanceSettings::default()
    );
}

#[test]
fn local_unix_access_requires_an_explicit_numeric_uid() {
    let mut values = required_values();
    values.insert("SYLVANDER_UNIX_UID".into(), "501".into());
    let config = ServerConfig::from_legacy_values(&values).unwrap();
    assert!(!config.agents[0].access.allow_authenticated);
    assert_eq!(
        config.agents[0].access.allowed_principals,
        ["unix:terminal:uid:501"]
    );

    values.insert("SYLVANDER_UNIX_UID".into(), "current-user".into());
    let error = ServerConfig::from_legacy_values(&values).unwrap_err();
    assert!(error.errors[0].contains("non-negative numeric uid"));
}

#[test]
fn legacy_environment_requires_explicit_self_use_mode() {
    let mut values = required_values();
    values.remove("SYLVANDER_MODE");
    let error = ServerConfig::from_legacy_values(&values).unwrap_err();
    assert!(error.errors[0].contains("SYLVANDER_MODE"));

    values.insert("SYLVANDER_MODE".into(), "production".into());
    let error = ServerConfig::from_legacy_values(&values).unwrap_err();
    assert!(error.errors[0].contains("production uses SYLVANDER_CONFIG"));
}

#[test]
fn alternate_models_and_optional_channels_are_migrated() {
    let mut values = required_values();
    values.insert("SYLVANDER_MODELS".into(), "model-b, model-a".into());
    values.insert("DINGTALK_APP_KEY".into(), "key".into());
    values.insert("DINGTALK_APP_SECRET".into(), "secret".into());
    values.insert("TELEGRAM_BOT_TOKEN".into(), "token".into());
    values.insert("TELEGRAM_WEBHOOK_SECRET".into(), "webhook-secret".into());
    let config = ServerConfig::from_legacy_values(&values).unwrap();
    assert_eq!(config.model_providers[0].models.len(), 2);
    assert_eq!(config.channels.len(), 4);
    assert_eq!(config.channels[2].id, "dingtalk-default");
    assert_eq!(config.channels[3].id, "telegram-default");
}

#[test]
fn partial_channel_credentials_fail_before_startup() {
    let mut values = required_values();
    values.insert("DINGTALK_APP_KEY".into(), "key".into());
    let error = ServerConfig::from_legacy_values(&values).unwrap_err();
    assert!(error.errors[0].contains("requires both"));
}
