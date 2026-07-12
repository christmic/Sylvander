use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

use super::{RequestRow, StatBucket};
use crate::config::expand_tilde;

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS requests (
  id                 TEXT PRIMARY KEY,
  ts                 INTEGER NOT NULL,
  client_protocol    TEXT NOT NULL,
  model_id           TEXT NOT NULL,
  provider           TEXT NOT NULL,
  real_model         TEXT NOT NULL,
  stream             INTEGER NOT NULL,
  status             INTEGER,
  input_tokens       INTEGER NOT NULL DEFAULT 0,
  output_tokens      INTEGER NOT NULL DEFAULT 0,
  cache_write_tokens INTEGER NOT NULL DEFAULT 0,
  cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
  latency_ms         INTEGER,
  ttft_ms            INTEGER,
  error              TEXT,
  tool               TEXT NOT NULL DEFAULT 'OTHER',
  tool_raw           TEXT
);
CREATE INDEX IF NOT EXISTS idx_requests_ts ON requests(ts);
CREATE INDEX IF NOT EXISTS idx_requests_provider ON requests(provider, real_model);

CREATE TABLE IF NOT EXISTS tool_rules (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  label      TEXT NOT NULL,
  header     TEXT NOT NULL,
  pattern    TEXT NOT NULL,
  priority   INTEGER NOT NULL DEFAULT 100,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS providers (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT NOT NULL UNIQUE,
  base_url   TEXT NOT NULL,
  dialect    TEXT NOT NULL,
  api_key    TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS logical_models (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  model_id     TEXT NOT NULL UNIQUE,
  provider_id  INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  real_model   TEXT NOT NULL,
  inject_usage INTEGER NOT NULL DEFAULT 0,
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS provider_rate_limits (
  provider           TEXT PRIMARY KEY,
  updated_at         INTEGER NOT NULL,
  requests_limit     INTEGER,
  requests_remaining INTEGER,
  requests_reset     TEXT,
  tokens_limit       INTEGER,
  tokens_remaining   INTEGER,
  tokens_reset       TEXT,
  raw                TEXT
);

CREATE TABLE IF NOT EXISTS settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS provider_keys (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  api_key     TEXT NOT NULL,
  label       TEXT,
  enabled     INTEGER NOT NULL DEFAULT 1,
  created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS routes (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  model_id   TEXT NOT NULL REFERENCES logical_models(model_id) ON DELETE CASCADE,
  provider   TEXT NOT NULL,
  real_model TEXT NOT NULL,
  weight     INTEGER NOT NULL DEFAULT 1,
  priority   INTEGER NOT NULL DEFAULT 100,
  enabled    INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL
);
"#;

#[derive(Clone)]
pub struct SqliteStore {
    pub(crate) pool: SqlitePool,
}

impl SqliteStore {
    pub async fn open(db_path: &str) -> anyhow::Result<Self> {
        let path = expand_tilde(db_path);
        if let Some(parent) = std::path::Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .foreign_keys(true)
            .create_if_missing(true);
        // In-memory DBs are per-connection; pin to one connection so the schema
        // (and data) persist across queries in the same process (tests).
        let in_memory = path == ":memory:" || path.contains(":memory:");
        let mut pool_opts = SqlitePoolOptions::new();
        if in_memory {
            pool_opts = pool_opts.max_connections(1).min_connections(1);
        }
        let pool = pool_opts.connect_with(opts).await?;
        sqlx::raw_sql(SCHEMA).execute(&pool).await?;
        // Best-effort migrations for DBs created before these columns existed.
        // Errors ("duplicate column name") are expected on already-migrated DBs.
        let _ = sqlx::query("ALTER TABLE requests ADD COLUMN tool TEXT NOT NULL DEFAULT 'OTHER'")
            .execute(&pool)
            .await;
        let _ = sqlx::query("ALTER TABLE requests ADD COLUMN tool_raw TEXT")
            .execute(&pool)
            .await;
        let _ = sqlx::query("ALTER TABLE requests ADD COLUMN attempts INTEGER NOT NULL DEFAULT 1")
            .execute(&pool)
            .await;
        let _ = sqlx::query("ALTER TABLE requests ADD COLUMN route_reason TEXT")
            .execute(&pool)
            .await;
        let _ = sqlx::query("ALTER TABLE requests ADD COLUMN route_trail TEXT")
            .execute(&pool)
            .await;

        let store = Self { pool };
        store.migrate_routing().await?;
        Ok(store)
    }

    /// One-time idempotent migration to the multi-target routing model:
    /// seed provider_keys from the legacy providers.api_key, and routes from
    /// legacy logical_models (provider_id + real_model).
    async fn migrate_routing(&self) -> anyhow::Result<()> {
        let pk: i64 = sqlx::query("SELECT COUNT(*) AS c FROM provider_keys")
            .fetch_one(&self.pool)
            .await?
            .get("c");
        if pk == 0 {
            sqlx::query(
                r#"INSERT INTO provider_keys (provider_id, api_key, label, enabled, created_at)
                   SELECT id, api_key, 'default', 1, CAST(strftime('%s','now') AS INTEGER)*1000
                   FROM providers WHERE api_key IS NOT NULL AND api_key <> ''"#,
            )
            .execute(&self.pool)
            .await?;
        }
        let r: i64 = sqlx::query("SELECT COUNT(*) AS c FROM routes")
            .fetch_one(&self.pool)
            .await?
            .get("c");
        if r == 0 {
            sqlx::query(
                r#"INSERT INTO routes (model_id, provider, real_model, weight, priority, enabled, created_at)
                   SELECT m.model_id, p.name, m.real_model, 1, 100, 1,
                          CAST(strftime('%s','now') AS INTEGER)*1000
                   FROM logical_models m JOIN providers p ON p.id = m.provider_id"#,
            )
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn record(&self, row: RequestRow) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO requests
               (id, ts, client_protocol, model_id, provider, real_model, stream, status,
                input_tokens, output_tokens, cache_write_tokens, cache_read_tokens,
                latency_ms, ttft_ms, error, tool, tool_raw, attempts, route_reason, route_trail)
               VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(row.id)
        .bind(row.ts)
        .bind(row.client_protocol)
        .bind(row.model_id)
        .bind(row.provider)
        .bind(row.real_model)
        .bind(row.stream as i64)
        .bind(row.status)
        .bind(row.input_tokens)
        .bind(row.output_tokens)
        .bind(row.cache_write_tokens)
        .bind(row.cache_read_tokens)
        .bind(row.latency_ms)
        .bind(row.ttft_ms)
        .bind(row.error)
        .bind(row.tool)
        .bind(row.tool_raw)
        .bind(row.attempts)
        .bind(row.route_reason)
        .bind(row.route_trail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Aggregate usage by provider + model + day. Optional inclusive [from, to] as YYYY-MM-DD.
    pub async fn stats(
        &self,
        from: Option<&str>,
        to: Option<&str>,
    ) -> anyhow::Result<Vec<StatBucket>> {
        let mut sql = String::from(
            r#"SELECT provider, real_model, tool,
                      strftime('%Y-%m-%d', ts/1000, 'unixepoch') AS date,
                      COUNT(*)                AS requests,
                      SUM(input_tokens)       AS input_tokens,
                      SUM(output_tokens)      AS output_tokens,
                      SUM(cache_read_tokens)  AS cache_read_tokens,
                      SUM(cache_write_tokens) AS cache_write_tokens
               FROM requests"#,
        );
        let mut conds: Vec<&str> = Vec::new();
        if from.is_some() {
            conds.push("date >= ?");
        }
        if to.is_some() {
            conds.push("date <= ?");
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        sql.push_str(
            " GROUP BY provider, real_model, tool, date ORDER BY date DESC, requests DESC",
        );

        // SQL is assembled from static fragments only; all values are bind params.
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        if let Some(f) = from {
            q = q.bind(f);
        }
        if let Some(t) = to {
            q = q.bind(t);
        }
        let rows = q.fetch_all(&self.pool).await?;

        let out = rows
            .into_iter()
            .map(|r| StatBucket {
                provider: r.get("provider"),
                real_model: r.get("real_model"),
                tool: r.get("tool"),
                date: r.get("date"),
                requests: r.get("requests"),
                input_tokens: r.get::<Option<i64>, _>("input_tokens").unwrap_or(0),
                output_tokens: r.get::<Option<i64>, _>("output_tokens").unwrap_or(0),
                cache_read_tokens: r.get::<Option<i64>, _>("cache_read_tokens").unwrap_or(0),
                cache_write_tokens: r.get::<Option<i64>, _>("cache_write_tokens").unwrap_or(0),
            })
            .collect();
        Ok(out)
    }
}
