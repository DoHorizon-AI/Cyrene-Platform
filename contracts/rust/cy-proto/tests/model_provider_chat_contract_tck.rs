// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-proto/tests/model_provider_chat_contract_tck.rs ║
// ║ Module: CYRENE Platform                                             ║
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary. ║
// ║                                                                      ║
// ║ 模块：CYRENE Platform                                                ║
// ║ 职责：Rust 实现、契约或一致性测试。                                   ║
// ╚══════════════════════════════════════════════════════════════════════╝

use cy_proto::{
    capability_v1::{
        invoke_capability_response, InvokeCapabilityRequest, InvokeCapabilityResponse,
    },
    model_provider::{
        CAPABILITY_ID, CHAT_COMPLETION_METHOD, CHAT_COMPLETION_REQUEST_TYPE_URL,
        CHAT_COMPLETION_RESPONSE_TYPE_URL, INTERFACE_VERSION,
    },
    model_provider_v1::{
        chat_message, ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse,
        ChatMessage,
    },
};
use prost::Message;
use prost_types::Any;

fn pack<M: Message>(type_url: &str, message: &M) -> Any {
    Any {
        type_url: type_url.to_string(),
        value: message.encode_to_vec(),
    }
}

fn unpack<M: Message + Default>(payload: &Any, type_url: &str) -> M {
    assert_eq!(payload.type_url, type_url);
    M::decode(payload.value.as_slice()).expect("typed Any payload must decode")
}

#[test]
fn chat_roles_are_stable_and_optional_fields_preserve_presence() {
    assert_eq!(chat_message::Role::Unspecified as i32, 0);
    assert_eq!(chat_message::Role::System as i32, 1);
    assert_eq!(chat_message::Role::User as i32, 2);
    assert_eq!(chat_message::Role::Assistant as i32, 3);
    assert_eq!(chat_message::Role::Tool as i32, 4);
    assert_eq!(
        chat_message::Role::Assistant.as_str_name(),
        "ROLE_ASSISTANT"
    );

    let request = ChatCompletionRequest {
        messages: vec![
            ChatMessage {
                role: chat_message::Role::System as i32,
                content: "follow the contract".to_string(),
                name: Some("policy".to_string()),
                tool_call_id: None,
            },
            ChatMessage {
                role: chat_message::Role::Tool as i32,
                content: "tool result".to_string(),
                name: None,
                tool_call_id: Some("call-1".to_string()),
            },
        ],
        model: Some("selected-model".to_string()),
        stream: true,
        temperature: Some(0.25),
        max_tokens: Some(64),
    };
    let decoded = ChatCompletionRequest::decode(request.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded, request);
    assert_eq!(decoded.messages[0].name.as_deref(), Some("policy"));
    assert_eq!(decoded.messages[0].tool_call_id, None);
    assert_eq!(decoded.messages[1].name, None);
    assert_eq!(decoded.messages[1].tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(decoded.temperature, Some(0.25));
    assert_eq!(decoded.max_tokens, Some(64));

    let omitted = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: chat_message::Role::User as i32,
            content: "hello".to_string(),
            name: None,
            tool_call_id: None,
        }],
        model: None,
        stream: false,
        temperature: None,
        max_tokens: None,
    };
    let decoded_omitted =
        ChatCompletionRequest::decode(omitted.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded_omitted.model, None);
    assert_eq!(decoded_omitted.temperature, None);
    assert_eq!(decoded_omitted.max_tokens, None);
}

#[test]
fn chat_any_round_trip_uses_canonical_method_and_type_urls() {
    assert_eq!(CAPABILITY_ID, "model.provider.v1");
    assert_eq!(INTERFACE_VERSION, "1");
    assert_eq!(CHAT_COMPLETION_METHOD, "chat_completion");
    assert_eq!(
        CHAT_COMPLETION_REQUEST_TYPE_URL,
        "type.googleapis.com/cyrene.model.provider.v1.ChatCompletionRequest"
    );
    assert_eq!(
        CHAT_COMPLETION_RESPONSE_TYPE_URL,
        "type.googleapis.com/cyrene.model.provider.v1.ChatCompletionResponse"
    );

    let request_payload = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: chat_message::Role::User as i32,
            content: "hello".to_string(),
            name: None,
            tool_call_id: None,
        }],
        model: None,
        stream: false,
        temperature: None,
        max_tokens: None,
    };
    let invoke = InvokeCapabilityRequest {
        capability: CAPABILITY_ID.to_string(),
        interface_version: INTERFACE_VERSION.to_string(),
        method: CHAT_COMPLETION_METHOD.to_string(),
        request: Some(pack(CHAT_COMPLETION_REQUEST_TYPE_URL, &request_payload)),
        binding_id: Some("openai-main".to_string()),
    };
    assert_eq!(
        unpack::<ChatCompletionRequest>(
            invoke.request.as_ref().unwrap(),
            CHAT_COMPLETION_REQUEST_TYPE_URL
        ),
        request_payload
    );

    let response_payload = ChatCompletionResponse {
        chunks: vec![
            ChatCompletionChunk {
                delta: "hello".to_string(),
                finish_reason: None,
                prompt_tokens: None,
                completion_tokens: None,
            },
            ChatCompletionChunk {
                delta: String::new(),
                finish_reason: Some("stop".to_string()),
                prompt_tokens: Some(2),
                completion_tokens: Some(1),
            },
        ],
    };
    let response = InvokeCapabilityResponse {
        result: Some(invoke_capability_response::Result::Response(pack(
            CHAT_COMPLETION_RESPONSE_TYPE_URL,
            &response_payload,
        ))),
    };
    let invoke_capability_response::Result::Response(payload) = response.result.unwrap() else {
        panic!("chat domain result must use the typed response branch");
    };
    let decoded_response =
        unpack::<ChatCompletionResponse>(&payload, CHAT_COMPLETION_RESPONSE_TYPE_URL);
    assert_eq!(decoded_response, response_payload);
    assert_eq!(decoded_response.chunks.len(), 2);
    assert_eq!(
        decoded_response.chunks[1].finish_reason.as_deref(),
        Some("stop")
    );
    assert_eq!(decoded_response.chunks[1].prompt_tokens, Some(2));
    assert_eq!(decoded_response.chunks[1].completion_tokens, Some(1));
}
