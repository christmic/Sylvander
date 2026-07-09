//! AskUserGate — mechanism passed into AgentLoop to pause for user input.

use async_trait::async_trait;

/// Gate that pauses the loop and asks the user a question.
///
/// The loop calls `ask()` and awaits the result. The implementation
/// publishes an event to the bus and waits for the user's reply.
///
/// `options` empty means free-text input. Otherwise the user picks
/// from the given choices (or multiple if `multi_select` is true).
#[async_trait]
pub trait AskUserGate: Send + Sync {
    /// Ask the user a question. Returns the user's selections.
    async fn ask(
        &self,
        call_id: &str,
        question: &str,
        options: Vec<String>,
        multi_select: bool,
    ) -> Vec<String>;
}
