use super::*;

async fn store() -> SqliteStore {
    SqliteStore::open(":memory:").await.unwrap()
}

#[tokio::test]
async fn provider_and_model_crud() {
    let s = store().await;
    s.add_provider(
        "anthropic",
        "https://api.anthropic.com",
        Dialect::Anthropic,
        Some("sk-1"),
    )
    .await
    .unwrap();
    s.add_provider(
        "deepseek",
        "https://api.deepseek.com",
        Dialect::OpenaiChat,
        Some("sk-2"),
    )
    .await
    .unwrap();

    let ps = s.list_providers().await.unwrap();
    assert_eq!(ps.len(), 2);

    s.add_model("claude-opus-4-6", "anthropic", "claude-opus-4-6", false)
        .await
        .unwrap();
    s.add_model("my-cheap-coder", "deepseek", "deepseek-chat", true)
        .await
        .unwrap();

    let ms = s.list_models().await.unwrap();
    assert_eq!(ms.len(), 2);

    let sets = s.load_routes().await.unwrap();
    assert_eq!(sets.len(), 2);
    let coder = sets
        .iter()
        .find(|r| r.model_id == "my-cheap-coder")
        .unwrap();
    assert!(coder.inject_usage);
    assert_eq!(coder.targets.len(), 1);
    let t = &coder.targets[0];
    assert_eq!(t.provider, "deepseek");
    assert_eq!(t.real_model, "deepseek-chat");
    assert_eq!(t.dialect, Dialect::OpenaiChat);
    assert_eq!(t.keys, vec!["sk-2".to_string()]);
}

#[tokio::test]
async fn multi_key_and_multi_target() {
    let s = store().await;
    s.add_provider("a", "https://a", Dialect::Anthropic, Some("k1"))
        .await
        .unwrap();
    s.add_provider_key("a", "k2", Some("second")).await.unwrap();
    s.add_provider("b", "https://b", Dialect::OpenaiChat, Some("kb"))
        .await
        .unwrap();
    s.add_model("m", "a", "real-a", false).await.unwrap(); // primary route
    s.add_route("m", "b", "real-b", 1, 200).await.unwrap(); // fallback tier

    let sets = s.load_routes().await.unwrap();
    let m = sets.iter().find(|r| r.model_id == "m").unwrap();
    assert_eq!(m.targets.len(), 2);
    // priority order: a (100) before b (200)
    assert_eq!(m.targets[0].provider, "a");
    assert_eq!(m.targets[0].keys.len(), 2); // k1 + k2
    assert_eq!(m.targets[1].provider, "b");
}

#[tokio::test]
async fn cascade_delete() {
    let s = store().await;
    s.add_provider("p", "https://x", Dialect::Anthropic, None)
        .await
        .unwrap();
    s.add_model("m", "p", "m", false).await.unwrap();
    assert_eq!(s.remove_provider("p").await.unwrap(), 1);
    assert_eq!(s.list_models().await.unwrap().len(), 0); // cascaded
}

#[tokio::test]
async fn add_model_unknown_provider_fails() {
    let s = store().await;
    assert!(s.add_model("m", "ghost", "m", false).await.is_err());
}

#[tokio::test]
async fn upsert_provider() {
    let s = store().await;
    s.add_provider("p", "https://a", Dialect::Anthropic, Some("k1"))
        .await
        .unwrap();
    s.add_provider("p", "https://b", Dialect::Anthropic, Some("k2"))
        .await
        .unwrap();
    let ps = s.list_providers().await.unwrap();
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0].base_url, "https://b");
    assert_eq!(ps[0].api_key.as_deref(), Some("k2"));
}
