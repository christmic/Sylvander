use std::collections::HashMap;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::Row;
use tracing::warn;

use super::{
    ModelRow, ObservedTool, ProviderKeyRow, ProviderRow, RateLimitRow, RouteRow, RouteSet,
    TargetDef, ToolRuleRow,
};
use crate::config::Dialect;
use crate::ratelimit::RateLimitSnapshot;
use crate::store::sqlite::SqliteStore;
use crate::tool::ToolRule;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Provider + logical-model persistence. Single choke-point for `api_key`
/// (a future encryption layer wraps read/write here without schema churn).
impl SqliteStore {
    // ---- providers ----

    /// Insert or update a provider by name.
    pub async fn add_provider(
        &self,
        name: &str,
        base_url: &str,
        dialect: Dialect,
        api_key: Option<&str>,
    ) -> anyhow::Result<()> {
        let ts = now_ms();
        sqlx::query(
            r#"INSERT INTO providers (name, base_url, dialect, api_key, created_at, updated_at)
               VALUES (?,?,?,?,?,?)
               ON CONFLICT(name) DO UPDATE SET
                 base_url = excluded.base_url,
                 dialect  = excluded.dialect,
                 api_key  = excluded.api_key,
                 updated_at = excluded.updated_at"#,
        )
        .bind(name)
        .bind(base_url)
        .bind(dialect.as_str())
        .bind(api_key)
        .bind(ts)
        .bind(ts)
        .execute(&self.pool)
        .await?;
        // Seed a key row so the provider is usable by the router (dedup by value).
        if let Some(key) = api_key {
            if !key.is_empty() {
                self.add_provider_key(name, key, Some("default")).await?;
            }
        }
        Ok(())
    }

    // ---- provider keys ----

    /// Add a key for a provider (skips if the same key value already exists).
    pub async fn add_provider_key(
        &self,
        provider_name: &str,
        api_key: &str,
        label: Option<&str>,
    ) -> anyhow::Result<()> {
        let provider_id: i64 = sqlx::query("SELECT id FROM providers WHERE name = ?")
            .bind(provider_name)
            .fetch_optional(&self.pool)
            .await?
            .map(|r| r.get::<i64, _>("id"))
            .ok_or_else(|| anyhow::anyhow!("unknown provider `{provider_name}`"))?;

        let exists: Option<i64> =
            sqlx::query("SELECT id FROM provider_keys WHERE provider_id = ? AND api_key = ?")
                .bind(provider_id)
                .bind(api_key)
                .fetch_optional(&self.pool)
                .await?
                .map(|r| r.get::<i64, _>("id"));
        if exists.is_some() {
            return Ok(());
        }
        sqlx::query(
            "INSERT INTO provider_keys (provider_id, api_key, label, enabled, created_at) VALUES (?,?,?,1,?)",
        )
        .bind(provider_id)
        .bind(api_key)
        .bind(label)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_provider_keys(&self, provider: Option<&str>) -> anyhow::Result<Vec<ProviderKeyRow>> {
        let rows = if let Some(p) = provider {
            sqlx::query(
                r#"SELECT pk.id, pr.name AS provider, pk.api_key, pk.label, pk.enabled
                   FROM provider_keys pk JOIN providers pr ON pr.id = pk.provider_id
                   WHERE pr.name = ? ORDER BY pk.id"#,
            )
            .bind(p)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"SELECT pk.id, pr.name AS provider, pk.api_key, pk.label, pk.enabled
                   FROM provider_keys pk JOIN providers pr ON pr.id = pk.provider_id
                   ORDER BY pr.name, pk.id"#,
            )
            .fetch_all(&self.pool)
            .await?
        };
        Ok(rows
            .into_iter()
            .map(|r| ProviderKeyRow {
                id: r.get("id"),
                provider: r.get("provider"),
                api_key: r.get("api_key"),
                label: r.get("label"),
                enabled: r.get::<i64, _>("enabled") != 0,
            })
            .collect())
    }

    pub async fn remove_provider_key(&self, id: i64) -> anyhow::Result<u64> {
        let res = sqlx::query("DELETE FROM provider_keys WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    pub async fn list_providers(&self) -> anyhow::Result<Vec<ProviderRow>> {
        let rows = sqlx::query(
            "SELECT name, base_url, dialect, api_key FROM providers ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| ProviderRow {
                name: r.get("name"),
                base_url: r.get("base_url"),
                dialect: r.get("dialect"),
                api_key: r.get("api_key"),
            })
            .collect())
    }

    /// Remove a provider (cascades to its logical models). Returns rows affected.
    pub async fn remove_provider(&self, name: &str) -> anyhow::Result<u64> {
        let res = sqlx::query("DELETE FROM providers WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    // ---- logical models ----

    /// Insert or update a logical model. Fails if the provider name is unknown.
    pub async fn add_model(
        &self,
        model_id: &str,
        provider_name: &str,
        real_model: &str,
        inject_usage: bool,
    ) -> anyhow::Result<()> {
        let provider_id: i64 = sqlx::query("SELECT id FROM providers WHERE name = ?")
            .bind(provider_name)
            .fetch_optional(&self.pool)
            .await?
            .map(|r| r.get::<i64, _>("id"))
            .ok_or_else(|| anyhow::anyhow!("unknown provider `{provider_name}`"))?;

        let ts = now_ms();
        sqlx::query(
            r#"INSERT INTO logical_models
               (model_id, provider_id, real_model, inject_usage, created_at, updated_at)
               VALUES (?,?,?,?,?,?)
               ON CONFLICT(model_id) DO UPDATE SET
                 provider_id  = excluded.provider_id,
                 real_model   = excluded.real_model,
                 inject_usage = excluded.inject_usage,
                 updated_at   = excluded.updated_at"#,
        )
        .bind(model_id)
        .bind(provider_id)
        .bind(real_model)
        .bind(inject_usage as i64)
        .bind(ts)
        .bind(ts)
        .execute(&self.pool)
        .await?;
        // Back-compat: ensure a primary route for this model exists.
        self.add_route(model_id, provider_name, real_model, 1, 100).await?;
        Ok(())
    }

    // ---- routes (targets) ----

    /// Add a route (target) for a logical model (skips exact duplicates).
    pub async fn add_route(
        &self,
        model_id: &str,
        provider: &str,
        real_model: &str,
        weight: i64,
        priority: i64,
    ) -> anyhow::Result<()> {
        let dup: Option<i64> = sqlx::query(
            "SELECT id FROM routes WHERE model_id=? AND provider=? AND real_model=?",
        )
        .bind(model_id)
        .bind(provider)
        .bind(real_model)
        .fetch_optional(&self.pool)
        .await?
        .map(|r| r.get::<i64, _>("id"));
        if dup.is_some() {
            return Ok(());
        }
        sqlx::query(
            "INSERT INTO routes (model_id, provider, real_model, weight, priority, enabled, created_at) VALUES (?,?,?,?,?,1,?)",
        )
        .bind(model_id)
        .bind(provider)
        .bind(real_model)
        .bind(weight)
        .bind(priority)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_routes(&self) -> anyhow::Result<Vec<RouteRow>> {
        let rows = sqlx::query(
            "SELECT id, model_id, provider, real_model, weight, priority, enabled FROM routes ORDER BY model_id, priority, id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| RouteRow {
                id: r.get("id"),
                model_id: r.get("model_id"),
                provider: r.get("provider"),
                real_model: r.get("real_model"),
                weight: r.get("weight"),
                priority: r.get("priority"),
                enabled: r.get::<i64, _>("enabled") != 0,
            })
            .collect())
    }

    pub async fn remove_route(&self, id: i64) -> anyhow::Result<u64> {
        let res = sqlx::query("DELETE FROM routes WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    pub async fn list_models(&self) -> anyhow::Result<Vec<ModelRow>> {
        let rows = sqlx::query(
            r#"SELECT m.model_id, p.name AS provider, m.real_model, m.inject_usage
               FROM logical_models m
               JOIN providers p ON p.id = m.provider_id
               ORDER BY m.model_id"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| ModelRow {
                model_id: r.get("model_id"),
                provider: r.get("provider"),
                real_model: r.get("real_model"),
                inject_usage: r.get::<i64, _>("inject_usage") != 0,
            })
            .collect())
    }

    pub async fn remove_model(&self, model_id: &str) -> anyhow::Result<u64> {
        let res = sqlx::query("DELETE FROM logical_models WHERE model_id = ?")
            .bind(model_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    // ---- routing ----

    /// Build the routing sets: per logical model, its ordered enabled targets,
    /// each target carrying its provider's enabled keys.
    pub async fn load_routes(&self) -> anyhow::Result<Vec<RouteSet>> {
        // provider name -> enabled keys
        let key_rows = sqlx::query(
            r#"SELECT p.name AS provider, pk.api_key
               FROM provider_keys pk JOIN providers p ON p.id = pk.provider_id
               WHERE pk.enabled = 1 ORDER BY pk.id"#,
        )
        .fetch_all(&self.pool)
        .await?;
        let mut keys: HashMap<String, Vec<String>> = HashMap::new();
        for r in key_rows {
            keys.entry(r.get("provider")).or_default().push(r.get("api_key"));
        }

        // inject_usage per model
        let model_rows = sqlx::query("SELECT model_id, inject_usage FROM logical_models")
            .fetch_all(&self.pool)
            .await?;
        let mut inject: HashMap<String, bool> = HashMap::new();
        for r in model_rows {
            inject.insert(r.get("model_id"), r.get::<i64, _>("inject_usage") != 0);
        }

        // enabled routes joined with provider connection details
        let rows = sqlx::query(
            r#"SELECT r.model_id, r.provider, r.real_model, r.weight, r.priority,
                      p.base_url, p.dialect
               FROM routes r JOIN providers p ON p.name = r.provider
               WHERE r.enabled = 1
               ORDER BY r.model_id, r.priority, r.id"#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut sets: HashMap<String, RouteSet> = HashMap::new();
        for r in rows {
            let model_id: String = r.get("model_id");
            let dialect_str: String = r.get("dialect");
            let dialect = match Dialect::from_str(&dialect_str) {
                Ok(d) => d,
                Err(e) => {
                    warn!(model = %model_id, error = %e, "skipping route with bad dialect");
                    continue;
                }
            };
            let provider: String = r.get("provider");
            let target = TargetDef {
                keys: keys.get(&provider).cloned().unwrap_or_default(),
                provider,
                base_url: r.get("base_url"),
                dialect,
                real_model: r.get("real_model"),
                weight: r.get("weight"),
                priority: r.get("priority"),
            };
            sets.entry(model_id.clone())
                .or_insert_with(|| RouteSet {
                    model_id: model_id.clone(),
                    inject_usage: inject.get(&model_id).copied().unwrap_or(false),
                    targets: Vec::new(),
                })
                .targets
                .push(target);
        }
        Ok(sets.into_values().collect())
    }

    // ---- rate limits ----

    /// Upsert the latest rate-limit snapshot for a provider.
    pub async fn upsert_rate_limit(
        &self,
        provider: &str,
        snap: &RateLimitSnapshot,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO provider_rate_limits
               (provider, updated_at, requests_limit, requests_remaining, requests_reset,
                tokens_limit, tokens_remaining, tokens_reset, raw)
               VALUES (?,?,?,?,?,?,?,?,?)
               ON CONFLICT(provider) DO UPDATE SET
                 updated_at         = excluded.updated_at,
                 requests_limit     = excluded.requests_limit,
                 requests_remaining = excluded.requests_remaining,
                 requests_reset     = excluded.requests_reset,
                 tokens_limit       = excluded.tokens_limit,
                 tokens_remaining   = excluded.tokens_remaining,
                 tokens_reset       = excluded.tokens_reset,
                 raw                = excluded.raw"#,
        )
        .bind(provider)
        .bind(now_ms())
        .bind(snap.requests_limit)
        .bind(snap.requests_remaining)
        .bind(snap.requests_reset.as_deref())
        .bind(snap.tokens_limit)
        .bind(snap.tokens_remaining)
        .bind(snap.tokens_reset.as_deref())
        .bind(&snap.raw)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- settings (key/value) ----

    pub async fn get_setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        let row = sqlx::query("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>("value")))
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO settings (key, value, updated_at) VALUES (?,?,?)
               ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at"#,
        )
        .bind(key)
        .bind(value)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Preset a setting only if it is not already present (first-run seed).
    pub async fn seed_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        if self.get_setting(key).await?.is_none() {
            self.set_setting(key, value).await?;
        }
        Ok(())
    }

    pub async fn list_settings(&self) -> anyhow::Result<Vec<(String, String)>> {
        let rows = sqlx::query("SELECT key, value FROM settings ORDER BY key")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| (r.get("key"), r.get("value"))).collect())
    }

    // ---- tool rules ----

    /// Add a tool-identification rule. Returns the new rule id.
    pub async fn add_tool_rule(
        &self,
        label: &str,
        header: &str,
        pattern: &str,
        priority: i64,
    ) -> anyhow::Result<i64> {
        let res = sqlx::query(
            r#"INSERT INTO tool_rules (label, header, pattern, priority, created_at)
               VALUES (?,?,?,?,?)"#,
        )
        .bind(label)
        .bind(header)
        .bind(pattern)
        .bind(priority)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(res.last_insert_rowid())
    }

    pub async fn list_tool_rules(&self) -> anyhow::Result<Vec<ToolRuleRow>> {
        let rows = sqlx::query(
            "SELECT id, label, header, pattern, priority FROM tool_rules ORDER BY priority, id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| ToolRuleRow {
                id: r.get("id"),
                label: r.get("label"),
                header: r.get("header"),
                pattern: r.get("pattern"),
                priority: r.get("priority"),
            })
            .collect())
    }

    pub async fn remove_tool_rule(&self, id: i64) -> anyhow::Result<u64> {
        let res = sqlx::query("DELETE FROM tool_rules WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// Ordered rules for the in-memory identification cache.
    pub async fn load_tool_rules(&self) -> anyhow::Result<Vec<ToolRule>> {
        Ok(self
            .list_tool_rules()
            .await?
            .into_iter()
            .map(|r| ToolRule {
                label: r.label,
                header: r.header,
                pattern: r.pattern,
            })
            .collect())
    }

    /// Seed default rules (claude-code, codex) if the table is empty.
    pub async fn seed_default_tool_rules(&self) -> anyhow::Result<()> {
        let count: i64 = sqlx::query("SELECT COUNT(*) AS c FROM tool_rules")
            .fetch_one(&self.pool)
            .await?
            .get("c");
        if count == 0 {
            self.add_tool_rule("claude-code", "user-agent", "claude-cli", 10).await?;
            self.add_tool_rule("codex", "originator", "codex", 10).await?;
            self.add_tool_rule("codex", "user-agent", "codex", 20).await?;
        }
        Ok(())
    }

    /// Distinct real tool identifiers seen in traffic, with logical mapping and
    /// count — surfaces unmapped tools (logical == "OTHER") for adding rules.
    pub async fn observed_tools(&self) -> anyhow::Result<Vec<ObservedTool>> {
        let rows = sqlx::query(
            r#"SELECT COALESCE(tool_raw, 'OTHER') AS tool_raw, tool, COUNT(*) AS requests
               FROM requests
               GROUP BY tool_raw, tool
               ORDER BY requests DESC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| ObservedTool {
                tool_raw: r.get("tool_raw"),
                tool: r.get("tool"),
                requests: r.get("requests"),
            })
            .collect())
    }

    pub async fn list_rate_limits(&self) -> anyhow::Result<Vec<RateLimitRow>> {
        let rows = sqlx::query(
            r#"SELECT provider, updated_at, requests_limit, requests_remaining, requests_reset,
                      tokens_limit, tokens_remaining, tokens_reset
               FROM provider_rate_limits ORDER BY provider"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| RateLimitRow {
                provider: r.get("provider"),
                updated_at: r.get("updated_at"),
                requests_limit: r.get("requests_limit"),
                requests_remaining: r.get("requests_remaining"),
                requests_reset: r.get("requests_reset"),
                tokens_limit: r.get("tokens_limit"),
                tokens_remaining: r.get("tokens_remaining"),
                tokens_reset: r.get("tokens_reset"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> SqliteStore {
        SqliteStore::open(":memory:").await.unwrap()
    }

    #[tokio::test]
    async fn provider_and_model_crud() {
        let s = store().await;
        s.add_provider("anthropic", "https://api.anthropic.com", Dialect::Anthropic, Some("sk-1"))
            .await
            .unwrap();
        s.add_provider("deepseek", "https://api.deepseek.com", Dialect::OpenaiChat, Some("sk-2"))
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
        let coder = sets.iter().find(|r| r.model_id == "my-cheap-coder").unwrap();
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
        s.add_provider("a", "https://a", Dialect::Anthropic, Some("k1")).await.unwrap();
        s.add_provider_key("a", "k2", Some("second")).await.unwrap();
        s.add_provider("b", "https://b", Dialect::OpenaiChat, Some("kb")).await.unwrap();
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
        s.add_provider("p", "https://x", Dialect::Anthropic, None).await.unwrap();
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
        s.add_provider("p", "https://a", Dialect::Anthropic, Some("k1")).await.unwrap();
        s.add_provider("p", "https://b", Dialect::Anthropic, Some("k2")).await.unwrap();
        let ps = s.list_providers().await.unwrap();
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].base_url, "https://b");
        assert_eq!(ps[0].api_key.as_deref(), Some("k2"));
    }
}
