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
        CHAT_COMPLETION_RESPONSE_TYPE_URL, CHAT_COMPLETION_V2_INTERFACE_VERSION,
        CHAT_COMPLETION_V2_METHOD, INTERFACE_VERSION,
    },
    model_provider_v1::{
        chat_message, ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse,
        ChatFunction, ChatMessage, ChatTool, ChatToolCall, ChatToolCallDelta, ChatToolCallFunction,
        ChatToolChoice,
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
                tool_calls: vec![],
            },
            ChatMessage {
                role: chat_message::Role::Tool as i32,
                content: "tool result".to_string(),
                name: None,
                tool_call_id: Some("call-1".to_string()),
                tool_calls: vec![],
            },
        ],
        model: Some("selected-model".to_string()),
        stream: true,
        temperature: Some(0.25),
        max_tokens: Some(64),
        tools: vec![],
        tool_choice: None,
        parallel_tool_calls: None,
        include_usage: None,
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
            tool_calls: vec![],
        }],
        model: None,
        stream: false,
        temperature: None,
        max_tokens: None,
        tools: vec![],
        tool_choice: None,
        parallel_tool_calls: None,
        include_usage: None,
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
            tool_calls: vec![],
        }],
        model: None,
        stream: false,
        temperature: None,
        max_tokens: None,
        tools: vec![],
        tool_choice: None,
        parallel_tool_calls: None,
        include_usage: None,
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
                role: None,
                tool_calls: vec![],
                total_tokens: None,
            },
            ChatCompletionChunk {
                delta: String::new(),
                finish_reason: Some("stop".to_string()),
                prompt_tokens: Some(2),
                completion_tokens: Some(1),
                role: None,
                tool_calls: vec![],
                total_tokens: None,
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

#[test]
fn structured_chat_v2_round_trip_preserves_tools_history_and_usage() {
    let request = ChatCompletionRequest {
        messages: vec![
            ChatMessage {
                role: chat_message::Role::Assistant as i32,
                content: String::new(),
                name: None,
                tool_call_id: None,
                tool_calls: vec![ChatToolCall {
                    id: "call-1".to_string(),
                    r#type: "function".to_string(),
                    function: Some(ChatToolCallFunction {
                        name: "weather".to_string(),
                        arguments: "{\"city\":\"Paris\"}".to_string(),
                    }),
                }],
            },
            ChatMessage {
                role: chat_message::Role::Tool as i32,
                content: "20C".to_string(),
                name: None,
                tool_call_id: Some("call-1".to_string()),
                tool_calls: vec![],
            },
        ],
        model: Some("selected-model".to_string()),
        stream: true,
        temperature: Some(0.2),
        max_tokens: Some(128),
        tools: vec![ChatTool {
            r#type: "function".to_string(),
            function: Some(ChatFunction {
                name: "weather".to_string(),
                description: Some("look up weather".to_string()),
                parameters_json: "{\"type\":\"object\"}".to_string(),
                strict: Some(true),
            }),
        }],
        tool_choice: Some(ChatToolChoice {
            mode: "function".to_string(),
            function_name: Some("weather".to_string()),
        }),
        parallel_tool_calls: Some(false),
        include_usage: Some(true),
    };
    let decoded = ChatCompletionRequest::decode(request.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded, request);
    assert_eq!(decoded.tools[0].function.as_ref().unwrap().name, "weather");
    assert_eq!(
        decoded
            .tool_choice
            .as_ref()
            .unwrap()
            .function_name
            .as_deref(),
        Some("weather")
    );
    assert_eq!(decoded.parallel_tool_calls, Some(false));
    assert_eq!(decoded.include_usage, Some(true));

    let response = ChatCompletionResponse {
        chunks: vec![ChatCompletionChunk {
            delta: String::new(),
            finish_reason: Some("tool_calls".to_string()),
            prompt_tokens: Some(8),
            completion_tokens: Some(4),
            role: Some("assistant".to_string()),
            tool_calls: vec![ChatToolCallDelta {
                index: 0,
                id: Some("call-1".to_string()),
                r#type: Some("function".to_string()),
                function_name: Some("weather".to_string()),
                function_arguments: Some("{\"city\":".to_string()),
            }],
            total_tokens: Some(12),
        }],
    };
    let decoded_response =
        ChatCompletionResponse::decode(response.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded_response, response);
    assert_eq!(CHAT_COMPLETION_V2_METHOD, "chat_completion_v2");
    assert_eq!(CHAT_COMPLETION_V2_INTERFACE_VERSION, "2");
}
