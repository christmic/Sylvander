use std::path::Path;

#[test]
fn runtime_source_tree_preserves_responsibility_boundaries() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    for directory in [
        "agent",
        "config",
        "credential",
        "evidence",
        "execution",
        "guardian",
        "mcp",
        "observability",
        "provider",
        "registry",
        "runtime",
        "session",
        "storage",
        "workspace",
    ] {
        assert!(
            source.join(directory).is_dir(),
            "Runtime responsibility must have a physical `{directory}/` directory"
        );
    }

    for legacy in [
        "agent_admin.rs",
        "agent_definition.rs",
        "agent_run.rs",
        "agent_supervisor.rs",
        "approval_store.rs",
        "boundary.rs",
        "coding_worktree.rs",
        "credential_audit.rs",
        "credential_registry.rs",
        "git_worktree.rs",
        "guardian_curation.rs",
        "guardian_runtime.rs",
        "identity_binding_service.rs",
        "mcp_stdio.rs",
        "model_registry.rs",
        "observability.rs",
        "principal_binding.rs",
        "prompt_contract.rs",
        "provider_registry.rs",
        "remote_git_worktree",
        "request_scoped_provider.rs",
        "self_change.rs",
        "session.rs",
    ] {
        assert!(
            !source.join(legacy).exists(),
            "legacy flat Runtime module `{legacy}` must stay inside its owning directory"
        );
    }

    for ambiguous in ["application", "domain", "infrastructure", "ports"] {
        assert!(
            !source.join(ambiguous).exists(),
            "top-level `{ambiguous}/` hides the Runtime business responsibility"
        );
    }
}

#[test]
fn crate_root_remains_a_small_public_facade() {
    let facade = include_str!("../src/lib.rs");
    assert!(
        facade.lines().count() <= 250,
        "sylvander-runtime/src/lib.rs must remain a facade, not regain Runtime implementation"
    );
    assert!(facade.contains("mod runtime;"));
    assert!(facade.contains("pub use runtime::{"));
}

#[test]
fn agent_run_keeps_construction_and_turn_orchestration_out_of_shared_state() {
    let run = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agent/run");
    for responsibility in [
        "background.rs",
        "builder.rs",
        "error.rs",
        "interaction.rs",
        "orchestration.rs",
        "projection.rs",
    ] {
        assert!(
            run.join(responsibility).is_file(),
            "Agent run responsibility must have a physical `{responsibility}` module"
        );
    }

    let shared_state = include_str!("../src/agent/run.rs");
    assert!(
        shared_state.lines().count() <= 2_000,
        "agent/run.rs must retain shared state instead of regaining construction or turn orchestration"
    );
}
