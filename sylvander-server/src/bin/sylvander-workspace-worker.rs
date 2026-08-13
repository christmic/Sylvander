//! Outbound macOS workspace worker.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("sylvander-workspace-worker is supported only on macOS");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = sylvander_runtime::workspace_worker_client::WorkspaceWorkerClientConfig {
        endpoint: required("SYLVANDER_WORKER_ENDPOINT")?,
        bearer_token: required("SYLVANDER_WORKER_TOKEN")?,
        target_id: required("SYLVANDER_WORKER_TARGET")?,
        workspace_root: required("SYLVANDER_WORKER_ROOT")?.into(),
        allow_local_fallback: std::env::var("SYLVANDER_WORKER_ALLOW_LOCAL_FALLBACK").as_deref()
            == Ok("true"),
    };
    sylvander_runtime::workspace_worker_client::run_workspace_worker(config).await?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{name} is required").into())
}
