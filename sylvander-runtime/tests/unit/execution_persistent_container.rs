use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use super::{ContainerPersistentProcessEnvironment, container_arguments};
use crate::execution::persistent::{
    PersistentFilesystemAuthority, PersistentNetworkAuthority, PersistentProcessAuthority,
    PersistentProcessEnvironment, PersistentProcessOwner, PersistentProcessSpec,
    PersistentResourceLimits,
};

fn authority(filesystem: PersistentFilesystemAuthority) -> PersistentProcessAuthority {
    PersistentProcessAuthority {
        owner: PersistentProcessOwner {
            principal_id: "user".into(),
            workload_id: "agent:mcp:search".into(),
            session_id: "session".into(),
            policy_revision: 1,
        },
        workspace_root: PathBuf::from("/srv/workspace"),
        filesystem,
        network: PersistentNetworkAuthority::Denied,
        resources: PersistentResourceLimits::default(),
        startup_timeout: Duration::from_secs(10),
        drain_timeout: Duration::from_secs(5),
    }
}

fn spec() -> PersistentProcessSpec {
    PersistentProcessSpec {
        program: "mcp-server".into(),
        arguments: vec!["--stdio".into()],
        environment: BTreeMap::from([("TOKEN".into(), "secret-value".into())]),
    }
}

#[test]
fn container_arguments_enforce_boundary_without_secret_values() {
    let arguments = container_arguments(
        "sylvander/mcp@sha256:abc",
        &spec(),
        &authority(PersistentFilesystemAuthority::WorkspaceRead),
        "/srv/workspace",
        "managed-process",
    )
    .into_iter()
    .map(|value| value.to_string_lossy().into_owned())
    .collect::<Vec<_>>();

    for required in [
        "--network=none",
        "--read-only",
        "no-new-privileges",
        "ALL",
        "--pids-limit",
        "mcp-server",
        "--stdio",
    ] {
        assert!(
            arguments.iter().any(|value| value == required),
            "{required}"
        );
    }
    assert!(arguments.iter().any(|value| value.ends_with(",readonly")));
    assert!(arguments.windows(2).any(|pair| pair == ["--env", "TOKEN"]));
    assert!(!arguments.iter().any(|value| value.contains("secret-value")));
}

#[test]
fn write_authority_does_not_make_workspace_mount_read_only() {
    let arguments = container_arguments(
        "image",
        &spec(),
        &authority(PersistentFilesystemAuthority::WorkspaceWrite),
        "/srv/workspace",
        "managed-process",
    );
    assert!(
        !arguments
            .iter()
            .any(|value| value.to_string_lossy().ends_with(",readonly"))
    );
}

#[test]
fn adapter_reports_only_controls_it_enforces() {
    let environment =
        ContainerPersistentProcessEnvironment::new("sandbox", "/usr/bin/docker", "image").unwrap();
    assert_eq!(environment.name(), "sandbox");
    assert!(environment.isolation().enforces_required_boundary());
}
