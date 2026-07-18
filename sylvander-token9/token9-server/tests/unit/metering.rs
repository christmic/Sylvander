use super::*;
use crate::config::Dialect;

#[test]
fn anthropic_streaming() {
    let sse = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude\",\"usage\":{\"input_tokens\":25,\"cache_creation_input_tokens\":10,\"cache_read_input_tokens\":5,\"output_tokens\":1}}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}

event: message_stop
data: {\"type\":\"message_stop\"}
";
    let u = parse(Dialect::Anthropic, sse.as_bytes());
    assert_eq!(u.input, 25);
    assert_eq!(u.output, 42);
    assert_eq!(u.cache_write, 10);
    assert_eq!(u.cache_read, 5);
}

#[test]
fn anthropic_non_streaming() {
    let json = r#"{"type":"message","model":"claude","content":[{"type":"text","text":"Hi"}],"usage":{"input_tokens":30,"output_tokens":15,"cache_creation_input_tokens":0,"cache_read_input_tokens":20}}"#;
    let u = parse(Dialect::Anthropic, json.as_bytes());
    assert_eq!(u.input, 30);
    assert_eq!(u.output, 15);
    assert_eq!(u.cache_write, 0);
    assert_eq!(u.cache_read, 20);
}

#[test]
fn openai_chat_streaming_with_usage() {
    let sse = "\
data: {\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"}}]}

data: {\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}

data: {\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":8,\"prompt_tokens_details\":{\"cached_tokens\":4}}}

data: [DONE]
";
    let u = parse(Dialect::OpenaiChat, sse.as_bytes());
    assert_eq!(u.input, 12);
    assert_eq!(u.output, 8);
    assert_eq!(u.cache_read, 4);
    assert_eq!(u.cache_write, 0);
}

#[test]
fn openai_responses_streaming() {
    let sse = "\
data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}

data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":50,\"output_tokens\":20,\"input_tokens_details\":{\"cached_tokens\":10}}}}
";
    let u = parse(Dialect::OpenaiResponses, sse.as_bytes());
    assert_eq!(u.input, 50);
    assert_eq!(u.output, 20);
    assert_eq!(u.cache_read, 10);
}

#[test]
fn openai_chat_non_streaming() {
    let json = r#"{"object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"Hi"}}],"usage":{"prompt_tokens":7,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":0}}}"#;
    let u = parse(Dialect::OpenaiChat, json.as_bytes());
    assert_eq!(u.input, 7);
    assert_eq!(u.output, 3);
}

#[test]
fn empty_or_unparseable_yields_zero() {
    let u = parse(Dialect::Anthropic, b"");
    assert_eq!(u.input, 0);
    assert_eq!(u.output, 0);
    let u2 = parse(Dialect::OpenaiChat, b"not json at all");
    assert_eq!(u2.input, 0);
}
