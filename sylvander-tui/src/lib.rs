//! Sylvander TUI — terminal client for Sylvander agents.
//!
//! Library surface for integration tests; the binary entry point lives in `bin/`.

pub mod app;
pub mod application;
pub mod approval_presenter;
pub mod client;
pub mod command;
pub mod compat;
pub mod component;
pub mod config;
pub mod diagnostics;
pub mod dirty;
pub mod event;
pub mod input;
pub mod keymap;
pub mod markdown;
pub mod modal;
pub mod model;
pub mod panel;
pub mod runtime;
pub mod service;
pub mod terminal_input;
pub mod theme;
pub mod tool_presenter;
pub mod ui;
pub mod workspace_service;
