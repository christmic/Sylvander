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
