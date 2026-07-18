use super::*;

#[test]
fn modal_stack_refuses_growth_beyond_its_hard_limit() {
    let mut stack = ModalStack::new();
    for index in 0..MAX_MODAL_STACK {
        assert!(stack.push(Box::new(ApprovalModal::new(
            format!("batch-{index}"),
            Vec::new(),
        ))));
    }
    assert!(!stack.push(Box::new(ApprovalModal::new("overflow".into(), Vec::new(),))));
    assert_eq!(stack.len(), MAX_MODAL_STACK);
}
