use super::*;

#[test]
fn production_requires_evidence_encryption_configuration() {
    let input = r#"
schema_version = 1
[server]
mode = "production"

[server.memory_maintenance.integrity]
[server.memory_maintenance.integrity.key]
source = "env"
name = "MEMORY_KEY"
[server.memory_maintenance.integrity.backend]
kind = "file"
anchor_path = "/var/lib/sylvander-integrity/anchor.json"
"#;

    let errors = ServerConfig::from_toml(input)
        .unwrap_err()
        .errors
        .join("\n");
    assert!(errors.contains("production mode requires evidence encryption-at-rest"));
}

#[test]
fn content_capture_requires_encryption_even_in_self_use_mode() {
    let input = r#"
schema_version = 1
[server]
mode = "self_use"
[server.evidence]
content = "redacted"
"#;

    let errors = ServerConfig::from_toml(input)
        .unwrap_err()
        .errors
        .join("\n");
    assert!(errors.contains("redacted or full evidence content requires encryption-at-rest"));
}

#[test]
fn governed_evidence_configuration_is_typed_and_valid() {
    let input = r#"
schema_version = 1
[server]
mode = "production"

[server.memory_maintenance.integrity]
[server.memory_maintenance.integrity.key]
source = "env"
name = "MEMORY_KEY"
[server.memory_maintenance.integrity.backend]
kind = "file"
anchor_path = "/var/lib/sylvander-integrity/anchor.json"

[server.evidence]
tenant_id = "tenant-a"
content = "full"
[server.evidence.encryption]
key_id = "evidence-key-2026-07"
[server.evidence.encryption.key]
source = "file"
path = "/run/secrets/sylvander-evidence-key"
"#;

    let config = ServerConfig::from_toml(input).expect("governed evidence configuration");
    assert_eq!(config.server.evidence.tenant_id, "tenant-a");
    let encryption = config.server.evidence.encryption.unwrap();
    assert_eq!(encryption.key_id, "evidence-key-2026-07");
    assert!(matches!(encryption.key, SecretRef::File { .. }));
}
