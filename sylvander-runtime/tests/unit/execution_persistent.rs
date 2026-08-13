use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use super::{
    PersistentFilesystemAuthority, PersistentNetworkAuthority, PersistentProcessAuthority,
    PersistentProcessEnvironment, PersistentProcessIsolation, PersistentProcessOwner,
    PersistentProcessSpec, PersistentResourceLimits, UnavailablePersistentProcessEnvironment,
};

fn authority() -> PersistentProcessAuthority {
    PersistentProcessAuthority {
        owner: PersistentProcessOwner {
            principal_id: "user-1".into(),
            workload_id: "agent-1:mcp:search".into(),
            session_id: "session-1".into(),
            policy_revision: 7,
        },
        workspace_root: PathBuf::from("/workspace"),
        filesystem: PersistentFilesystemAuthority::WorkspaceRead,
        network: PersistentNetworkAuthority::Denied,
        resources: PersistentResourceLimits::default(),
        startup_timeout: Duration::from_secs(10),
        drain_timeout: Duration::from_secs(5),
    }
}

fn spec() -> PersistentProcessSpec {
    PersistentProcessSpec {
        program: "server".into(),
        arguments: vec!["--stdio".into()],
        environment: BTreeMap::new(),
    }
}

#[tokio::test]
async fn unavailable_environment_fails_closed_after_validation() {
    let environment = UnavailablePersistentProcessEnvironment::new("local");
    let Err(error) = environment.spawn(&spec(), &authority()).await else {
        panic!("unavailable environment must reject spawn");
    };

    assert_eq!(environment.name(), "local");
    assert!(error.to_string().contains("unavailable"));
}

#[test]
fn isolation_truth_requires_every_enforcement_dimension() {
    let complete = PersistentProcessIsolation {
        filesystem: true,
        network_denied: true,
        resource_limits: true,
        process_tree: true,
    };
    let incomplete = PersistentProcessIsolation {
        network_denied: false,
        ..complete
    };

    assert!(complete.enforces_required_boundary());
    assert!(!incomplete.enforces_required_boundary());
}

#[tokio::test]
async fn invalid_owner_is_rejected_before_backend_selection() {
    let environment = UnavailablePersistentProcessEnvironment::new("sandbox");
    let mut invalid = authority();
    invalid.owner.session_id.clear();

    let Err(error) = environment.spawn(&spec(), &invalid).await else {
        panic!("invalid owner must reject spawn");
    };
    assert!(error.to_string().contains("owner"));
}
