use sylvander_llm_core::ChatMessage;

use super::ConversationSnapshot;

#[test]
fn snapshot_owns_model_messages_without_session_identity() {
    let snapshot = ConversationSnapshot::new(vec![ChatMessage::user("hello")]);

    assert_eq!(snapshot.messages().len(), 1);
    assert_eq!(snapshot.into_messages(), vec![ChatMessage::user("hello")]);
}
