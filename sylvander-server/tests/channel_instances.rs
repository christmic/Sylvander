#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use sylvander_runtime::config::{ChannelTransportConfig, SecretRef, ServerConfig};
use sylvander_runtime::credential_audit::{
    CredentialAuditOperation, CredentialAuditSubject, CredentialOperationAuditLedger,
};

const ALPHA_SECRET_NAME: &str = "SYLVANDER_E6_HTTP_ALPHA_TOKEN";
const ALPHA_SECRET: &str = "e6-alpha-secret";
const BETA_SECRET_NAME: &str = "SYLVANDER_E6_HTTP_BETA_TOKEN";
const BETA_SECRET: &str = "e6-beta-secret";
const PROVIDER_SECRET_NAME: &str = "SYLVANDER_E6_PROVIDER_KEY";

struct ServerProcess {
    child: Child,
    log_path: PathBuf,
}

impl ServerProcess {
    fn spawn(config_path: &Path, log_path: &Path) -> Self {
        let log = File::create(log_path).expect("create server lifecycle log");
        let child = Command::new(env!("CARGO_BIN_EXE_sylvander"))
            .env("SYLVANDER_CONFIG", config_path)
            .env(PROVIDER_SECRET_NAME, "e6-provider-secret")
            .env(ALPHA_SECRET_NAME, ALPHA_SECRET)
            .env(BETA_SECRET_NAME, BETA_SECRET)
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().expect("clone server lifecycle log"),
            ))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawn production Sylvander server");
        Self {
            child,
            log_path: log_path.to_owned(),
        }
    }

    fn wait_until_ready(&mut self, addresses: [SocketAddr; 2]) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut observations = [
            Err("not requested".to_string()),
            Err("not requested".to_string()),
        ];
        loop {
            if let Some(status) = self.child.try_wait().expect("poll server process") {
                panic!(
                    "server exited before both channel instances were ready ({status}):\n{}",
                    self.logs()
                );
            }
            for (index, address) in addresses.iter().enumerate() {
                observations[index] = http_request(*address, "GET", "/health", None, "")
                    .map_err(|error| error.to_string());
            }
            let ready = observations.iter().all(|response| {
                response.as_ref().is_ok_and(|(status, body)| {
                    *status == 200 && body.contains("\"total_channels\":2")
                })
            });
            if ready {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "both HTTP channel instances did not become ready; \
                 last responses={observations:?}:\n{}",
                self.logs()
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn interrupt_and_wait(&mut self) -> ExitStatus {
        let signal = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status()
            .expect("send SIGINT to production server");
        assert!(signal.success(), "SIGINT command failed: {signal}");

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().expect("poll graceful shutdown") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "server did not complete graceful shutdown:\n{}",
                self.logs()
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn logs(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[tokio::test]
async fn production_server_runs_same_kind_channels_with_isolated_credentials() {
    let temporary = tempfile::tempdir().expect("create E6 server workspace");
    let data_dir = temporary.path().join("data");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create Agent workspace");

    let first_listener = TcpListener::bind("127.0.0.1:0").expect("reserve first HTTP address");
    let second_listener = TcpListener::bind("127.0.0.1:0").expect("reserve second HTTP address");
    let alpha_address = first_listener.local_addr().expect("first HTTP address");
    let beta_address = second_listener.local_addr().expect("second HTTP address");
    drop(first_listener);
    drop(second_listener);

    let config_text = server_config(
        temporary.path(),
        &data_dir,
        &workspace,
        alpha_address,
        beta_address,
    );
    let config = ServerConfig::from_toml(&config_text).expect("parse real ServerConfig");
    assert_eq!(config.channels[0].id, "http-alpha");
    assert_eq!(config.channels[1].id, "http-beta");
    assert!(matches!(
        &config.channels[0].transport,
        ChannelTransportConfig::Http {
            bearer_token: SecretRef::Env { name },
            ..
        } if name == ALPHA_SECRET_NAME
    ));
    assert!(matches!(
        &config.channels[1].transport,
        ChannelTransportConfig::Http {
            bearer_token: SecretRef::Env { name },
            ..
        } if name == BETA_SECRET_NAME
    ));

    let config_path = temporary.path().join("server.toml");
    std::fs::write(&config_path, config_text).expect("write real ServerConfig");
    let log_path = temporary.path().join("server.log");
    let mut server = ServerProcess::spawn(&config_path, &log_path);
    server.wait_until_ready([alpha_address, beta_address]);

    assert_eq!(
        authenticated_invalid_json(alpha_address, ALPHA_SECRET),
        422,
        "alpha must accept only its own credential before JSON validation"
    );
    assert_eq!(
        authenticated_invalid_json(alpha_address, BETA_SECRET),
        401,
        "beta's secret must not authenticate to alpha"
    );
    assert_eq!(
        authenticated_invalid_json(beta_address, BETA_SECRET),
        422,
        "beta must accept only its own credential before JSON validation"
    );
    assert_eq!(
        authenticated_invalid_json(beta_address, ALPHA_SECRET),
        401,
        "alpha's secret must not authenticate to beta"
    );

    let status = server.interrupt_and_wait();
    assert!(status.success(), "server shutdown failed: {status}");
    let logs = server.logs();
    for instance in ["http-alpha", "http-beta"] {
        assert!(
            logs.contains("channel configured") && logs.contains(&format!("instance={instance}")),
            "production composition did not configure {instance}:\n{logs}"
        );
        assert!(
            logs.contains("channel ready") && logs.contains(&format!("instance={instance}")),
            "Runtime did not report {instance} ready:\n{logs}"
        );
        assert!(
            logs.contains("channel stopped") && logs.contains(&format!("instance={instance}")),
            "Runtime did not drain {instance}:\n{logs}"
        );
    }
    assert!(logs.contains("runtime shut down"));
    assert!(!logs.contains(ALPHA_SECRET));
    assert!(!logs.contains(BETA_SECRET));

    let audit = CredentialOperationAuditLedger::open(data_dir.join("credential-operations.db"))
        .await
        .expect("open production credential audit");
    let alpha_events = audit
        .list(
            &CredentialAuditSubject::channel_instance("http-alpha")
                .expect("valid alpha audit subject"),
            10,
        )
        .await
        .expect("read alpha credential audit");
    let beta_events = audit
        .list(
            &CredentialAuditSubject::channel_instance("http-beta")
                .expect("valid beta audit subject"),
            10,
        )
        .await
        .expect("read beta credential audit");

    for events in [&alpha_events, &beta_events] {
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .any(|event| event.operation == CredentialAuditOperation::Create)
        );
        assert!(
            events
                .iter()
                .any(|event| event.operation == CredentialAuditOperation::Renew)
        );
        assert!(
            events
                .iter()
                .all(|event| event.credential_revision == Some(1))
        );
    }
    assert!(alpha_events.iter().all(|alpha| {
        beta_events
            .iter()
            .all(|beta| alpha.event_id != beta.event_id)
    }));
}

fn authenticated_invalid_json(address: SocketAddr, token: &str) -> u16 {
    http_request(address, "POST", "/chat", Some(token), "{}")
        .expect("HTTP channel response")
        .0
}

fn http_request(
    address: SocketAddr,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: &str,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(250))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let authorization = bearer
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\n{authorization}\
         Content-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::other("HTTP response has no status"))?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    Ok((status, body))
}

fn server_config(
    root: &Path,
    data_dir: &Path,
    workspace: &Path,
    alpha_address: SocketAddr,
    beta_address: SocketAddr,
) -> String {
    format!(
        r#"
schema_version = 1

[server]
name = "e6-channel-instances"
data_dir = {data_dir}

[[model_providers]]
id = "fixture"
kind = "anthropic_compatible"
base_url = "http://127.0.0.1:9"

[model_providers.api_key]
source = "env"
name = "{PROVIDER_SECRET_NAME}"

[[model_providers.models]]
id = "fixture-model"
context_window = 32768
max_output_tokens = 4096
capabilities = ["tool_use"]

[[execution_targets]]
id = "local"

[execution_targets.transport]
kind = "local"
root = {root}

[[agents]]
revision = 1
allow_session_prompt = false

[agents.access]
allow_authenticated = true

[agents.spec]
id = "sylvander"
name = "Sylvander"

[agents.spec.persona]
system_prompt = "E6 same-kind channel lifecycle verification."
description = "E6 channel verification"

[agents.spec.model]
provider = "fixture"
model_name = "fixture-model"
allowed_models = [{{ provider_id = "fixture", model_id = "fixture-model" }}]
max_tokens = 4096

[agents.agent_workspace]
execution_target = "local"
path = {workspace}
read_only = false

[[channels]]
id = "http-alpha"
enabled = true
default_agent = "sylvander"

[channels.transport]
kind = "http"
bind = "{alpha_address}"
principal_id = "principal-alpha"

[channels.transport.bearer_token]
source = "env"
name = "{ALPHA_SECRET_NAME}"

[[channels]]
id = "http-beta"
enabled = true
default_agent = "sylvander"

[channels.transport]
kind = "http"
bind = "{beta_address}"
principal_id = "principal-beta"

[channels.transport.bearer_token]
source = "env"
name = "{BETA_SECRET_NAME}"
"#,
        root = toml_string(root),
        data_dir = toml_string(data_dir),
        workspace = toml_string(workspace),
    )
}

fn toml_string(path: &Path) -> String {
    serde_json::to_string(
        path.to_str()
            .expect("temporary path must be valid UTF-8 for TOML"),
    )
    .expect("serialize TOML path")
}
