use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn request_binds_session_token_kind_and_target() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("host.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut line = String::new();
        BufReader::new(read).read_line(&mut line).await.unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["session_id"], "session-a");
        assert_eq!(value["token"], "secret-capability");
        assert_eq!(value["kind"], "image");
        assert_eq!(value["target"], "art/output.png");
        write
            .write_all(b"{\"ok\":true,\"message\":\"Opened image\"}\n")
            .await
            .unwrap();
    });
    let config = HostBridgeConfig {
        socket_path: path,
        token: "secret-capability".into(),
    };

    let message = request_preview(&config, "session-a", PreviewKind::Image, "art/output.png")
        .await
        .unwrap();

    assert_eq!(message, "Opened image");
    server.await.unwrap();
}

#[tokio::test]
async fn rejection_message_is_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("host.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 1024];
        let _ = stream.read(&mut buffer).await.unwrap();
        stream
            .write_all(b"{\"ok\":false,\"message\":\"invalid session capability\"}\n")
            .await
            .unwrap();
    });
    let config = HostBridgeConfig {
        socket_path: path,
        token: "wrong".into(),
    };

    let error = request_preview(
        &config,
        "session-b",
        PreviewKind::Web,
        "https://example.com",
    )
    .await
    .unwrap_err();

    assert_eq!(error, "invalid session capability");
    server.await.unwrap();
}
