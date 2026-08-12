//! Executable negative-dependency rules for the Agent execution kernel.
//!
//! Cargo cannot express dependencies for a subset of one crate. Until the
//! remaining Runtime-owned modules leave `sylvander-agent`, this test prevents
//! the already-neutral kernel from regaining API, Runtime, or provider-adapter
//! imports during ordinary refactoring.

use std::fs;
use std::path::{Path, PathBuf};

const KERNEL_MODULES: &[&str] = &[
    "conversation.rs",
    "error.rs",
    "event.rs",
    "execution_context.rs",
    "execution_ports.rs",
    "loop_.rs",
    "outcome.rs",
    "plan_gate.rs",
    "request.rs",
    "tool.rs",
    "tool_context.rs",
    "tool_invocation.rs",
];

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

#[test]
fn execution_kernel_has_no_product_or_provider_dependencies() {
    for module in KERNEL_MODULES {
        let path = source_root().join(module);
        let source = fs::read_to_string(&path).expect("kernel module must be readable");
        for forbidden in [
            "sylvander_protocol",
            "sylvander_runtime",
            "sylvander_llm_anthropic",
            "sylvander_llm_openai",
            "sylvander_llm_dashscope",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} imports forbidden dependency `{forbidden}`",
                path.display()
            );
        }
    }
}

#[test]
fn production_agent_sources_have_no_provider_adapter_imports() {
    let mut pending = vec![source_root()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("source directory must be readable") {
            let path = entry.expect("source entry must be readable").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            let source = fs::read_to_string(&path).expect("Rust source must be readable");
            for forbidden in [
                "sylvander_llm_anthropic",
                "sylvander_llm_openai",
                "sylvander_llm_dashscope",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "{} imports provider adapter `{forbidden}`",
                    path.display()
                );
            }
        }
    }
}
