use super::*;

#[test]
fn maintained_local_example_matches_the_latest_schema() {
    let input = include_str!("../../../config/sylvander.local.example.toml");
    let config = ServerConfig::from_toml(input).expect("valid local example");

    assert_eq!(config.server.mode, ServerMode::SelfUse);
    assert_eq!(config.agents.len(), 1);
    assert_eq!(config.channels.len(), 1);
    assert!(config.server.memory_maintenance.integrity.key.is_none());
    assert!(config.server.evidence.encryption.is_none());
}
