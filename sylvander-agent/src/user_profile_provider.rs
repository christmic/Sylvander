//! Runtime-injected access to the authenticated user's current profile.

use async_trait::async_trait;
use sylvander_protocol::{AgentId, SessionId, UserId, UserProfileView};

/// Runtime-derived query subject. External callers may inspect it to perform
/// the lookup, but cannot construct it or replace its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserProfileSubject {
    user: UserId,
    agent: AgentId,
    session: SessionId,
}

impl UserProfileSubject {
    pub(crate) fn authenticated(user_id: UserId, agent_id: AgentId, session_id: SessionId) -> Self {
        Self {
            user: user_id,
            agent: agent_id,
            session: session_id,
        }
    }

    #[must_use]
    pub fn user_id(&self) -> &UserId {
        &self.user
    }

    #[must_use]
    pub fn agent_id(&self) -> &AgentId {
        &self.agent
    }

    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session
    }
}

/// Content-safe provider failure. Backend errors and profile values must stay
/// behind the Runtime boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UserProfileProviderError {
    #[error("user profile is unavailable")]
    Unavailable,
}

/// Object-safe live profile source owned and injected by Runtime.
#[async_trait]
pub trait UserProfileProvider: Send + Sync {
    /// Return the latest profile revision for this authenticated subject.
    /// `None` means no profile exists and is not an error.
    async fn current_profile(
        &self,
        subject: &UserProfileSubject,
    ) -> Result<Option<UserProfileView>, UserProfileProviderError>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    struct Absent;

    #[async_trait]
    impl UserProfileProvider for Absent {
        async fn current_profile(
            &self,
            _subject: &UserProfileSubject,
        ) -> Result<Option<UserProfileView>, UserProfileProviderError> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn provider_is_object_safe_and_subject_is_runtime_derived() {
        let provider: Arc<dyn UserProfileProvider> = Arc::new(Absent);
        let subject = UserProfileSubject::authenticated(
            UserId::new("stable-user"),
            AgentId::new("agent"),
            SessionId::new("session"),
        );
        assert_eq!(subject.user_id().0, "stable-user");
        assert!(provider.current_profile(&subject).await.unwrap().is_none());
    }

    #[test]
    fn errors_are_content_safe() {
        assert_eq!(
            UserProfileProviderError::Unavailable.to_string(),
            "user profile is unavailable"
        );
    }
}
