//! Sylvander TUI — terminal client for Sylvander agents.
//!
//! Library surface for integration tests; the binary entry point lives in `bin/`.

/// Persistent application state and pure state transitions.
pub mod app;
/// Top-level application orchestrator for input, service events, and drawing.
pub mod application;
/// Approval-specific presentation models and copy.
pub mod approval_presenter;
/// Unix protocol client and reconnecting service transport.
pub mod client;
/// Slash-command catalog, parsing, ranking, and execution.
pub mod command;
/// Terminal-width breakpoints and compact interaction affordances.
pub mod compat;
/// Reusable Ratatui view components.
pub mod component;
/// Runtime configuration, theme selection, and host-bridge settings.
pub mod config;
/// Content-safe local diagnostics and user-visible recovery hints.
pub mod diagnostics;
/// Dirty-state tracking for drafts and local mutations.
pub mod dirty;
/// Typed intents and service events consumed by the application loop.
pub mod event;
/// Optional authenticated Ghostty host-capability bridge.
pub mod host_bridge;
/// Unicode-safe editor state, cursor movement, and text insertion.
pub mod input;
/// Key binding lookup and intent mapping.
pub mod keymap;
/// Bounded Markdown-to-terminal-span rendering.
pub mod markdown;
/// Modal state and keyboard ownership for transient decisions.
pub mod modal;
/// UI-owned transcript, tool, session, and runtime presentation models.
pub mod model;
/// Side panels and compact supplemental views.
pub mod panel;
/// Terminal setup, frame loop, and teardown.
pub mod runtime;
/// Service abstraction that separates UI state from protocol transport.
pub mod service;
/// Crossterm input translation and input-thread lifecycle.
pub mod terminal_input;
/// Semantic color palette and configurable visual themes.
pub mod theme;
/// Compact, expanded, and diff-aware tool-result presentation.
pub mod tool_presenter;
/// Main Ratatui layout and transcript/composer rendering.
pub mod ui;
/// Read-only workspace-diff service used by review views.
pub mod workspace_service;
