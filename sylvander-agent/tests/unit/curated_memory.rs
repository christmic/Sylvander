use super::*;

#[test]
fn curated_scope_is_explicit_and_not_owner_bearing() {
    let proposal = MemoryCandidateSubmission {
        scope: CuratedMemoryScope::WorkspaceKnowledge,
        content: "Rust workspace".into(),
        tags: vec!["architecture".into()],
    };

    assert_eq!(proposal.scope, CuratedMemoryScope::WorkspaceKnowledge);
    assert!(!proposal.content.is_empty());
}
