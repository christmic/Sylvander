//! Built-in tool implementations.
//!
//! Runtime tools implement the shared [`crate::tool::Tool`] contract. Test
//! doubles are kept in the crate's `tests/` tree.

pub mod ask_user;
pub mod background_task;
pub mod command;
pub mod edit;
pub mod git;
pub mod list;
pub mod memory;
pub mod memory_read;
pub mod memory_sqlite;
pub mod memory_write;
pub mod plan;
pub mod read;
pub mod search;
pub mod update_plan;
pub mod write;

pub use ask_user::AskUserTool;
pub use background_task::StartBackgroundTaskTool;
pub use command::CommandTool;
pub use edit::EditTool;
pub use git::GitTool;
pub use list::ListTool;
pub use memory::{
    InMemoryMemoryStore, MemoryActorKind, MemoryAppend, MemoryEntry, MemoryExecutionContext,
    MemoryExpiryPatch, MemoryOwner, MemoryPatch, MemoryProvenance, MemoryProvenanceSource,
    MemoryScope, MemoryStore, MemoryStoreError, RelationshipMemoryRetentionPolicy,
};
pub use memory_read::MemoryReadTool;
pub use memory_sqlite::{
    FileMemoryIntegrityAnchor, HttpMemoryIntegrityAnchor, HttpMemoryIntegrityAnchorConfig,
    MemoryBackupArtifact, MemoryBackupManifest, MemoryClock, MemoryEvidenceCheckpoint,
    MemoryEvidenceCompactionReport, MemoryIntegrityConfig, MemoryPurgeReport, MemoryRestoreError,
    SqliteMemoryAdmin, SqliteMemoryMaintenance, SqliteMemoryStore, SystemMemoryClock,
};
pub use memory_write::MemoryWriteTool;
pub use plan::PresentPlanTool;
pub use read::ReadTool;
pub use search::SearchTool;
pub use update_plan::UpdatePlanTool;
pub use write::WriteTool;
