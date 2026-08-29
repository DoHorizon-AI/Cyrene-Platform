use std::collections::HashSet;

use cy_proto::capability_v1::{
    invoke_capability_response, CapabilityApplicationEvent, InvokeCapabilityRequest,
    InvokeCapabilityResponse,
};
use cy_proto::message_connector::{
    CAPABILITY_ID, DELIVERY_RESULT_TYPE_URL, INBOUND_MESSAGE_EVENT_TYPE, INBOUND_MESSAGE_TYPE_URL,
    INTERFACE_VERSION, MAX_VENDOR_FACTS, MAX_VENDOR_FACT_NAME_BYTES, MAX_VENDOR_FACT_TOTAL_BYTES,
    MAX_VENDOR_FACT_VALUE_BYTES, MAX_VENDOR_ID_BYTES, SEND_MESSAGE_METHOD,
    SEND_MESSAGE_REQUEST_TYPE_URL,
};
use cy_proto::message_connector_v1::{
    attachment_reference, message_content_part, AttachmentReference, ConversationKind,
    ConversationScope, DeliveryResult, DeliveryStatus, FileContent, ImageContent,
    InboundMessagePayload, MentionContent, MentionTarget, MessageContentPart, ReplyReference,
    SendMessageRequest, TextContent, VendorExtension, VendorFact, VendorMediaReference,
};
use prost::Message;
use prost_types::{Any, Duration};

fn pack<M: Message>(type_url: &str, value: &M) -> Any {
    Any {
        type_url: type_url.to_string(),
        value: value.encode_to_vec(),
    }
}

fn unpack<M: Message + Default>(value: &Any, expected_type_url: &str) -> M {
    assert_eq!(value.type_url, expected_type_url);
    M::decode(value.value.as_slice()).expect("typed Any payload must decode")
}

fn onebot_group_scope() -> ConversationScope {
    ConversationScope {
        vendor: "onebot.v11".to_string(),
        account_id: "10001".to_string(),
        conversation_id: "456".to_string(),
        kind: ConversationKind::Group as i32,
    }
}

fn vendor_extension() -> VendorExtension {
    VendorExtension {
        vendor: "onebot.v11".to_string(),
        facts: vec![
            VendorFact {
                name: "post_type".to_string(),
                value: "message".to_string(),
            },
            VendorFact {
                name: "sub_type".to_string(),
                value: "normal".to_string(),
            },
        ],
    }
}

fn inbound_fixture() -> InboundMessagePayload {
    InboundMessagePayload {
        message_id: "9002".to_string(),
        conversation: Some(onebot_group_scope()),
        sender_id: "123".to_string(),
        sender_display_name: "Alice".to_string(),
        content: vec![
            MessageContentPart {
                kind: Some(message_content_part::Kind::Mention(MentionContent {
                    target: MentionTarget::User as i32,
                    target_id: "654321".to_string(),
                    display_name: "Carol".to_string(),
                })),
            },
            MessageContentPart {
                kind: Some(message_content_part::Kind::Text(TextContent {
                    text: "look".to_string(),
                })),
            },
            MessageContentPart {
                kind: Some(message_content_part::Kind::Image(ImageContent {
                    reference: Some(AttachmentReference {
                        location: Some(attachment_reference::Location::RemoteUri(
                            "https://example.test/image.png".to_string(),
                        )),
                    }),
                    mime_type: "image/png".to_string(),
                })),
            },
            MessageContentPart {
                kind: Some(message_content_part::Kind::File(FileContent {
                    reference: Some(AttachmentReference {
                        location: Some(attachment_reference::Location::VendorMedia(
                            VendorMediaReference {
                                vendor: "onebot.v11".to_string(),
                                account_id: "10001".to_string(),
                                media_id: "report.pdf".to_string(),
                            },
                        )),
                    }),
                    file_name: "report.pdf".to_string(),
                    mime_type: "application/pdf".to_string(),
                })),
            },
        ],
        reply: Some(ReplyReference {
            message_id: "777".to_string(),
        }),
        vendor_extension: Some(vendor_extension()),
    }
}

fn valid_vendor_extension(extension: &VendorExtension) -> bool {
    if extension.vendor.is_empty()
        || extension.vendor.len() > MAX_VENDOR_ID_BYTES
        || extension.facts.len() > MAX_VENDOR_FACTS
    {
        return false;
    }

    let mut names = HashSet::new();
    let mut total_bytes = 0usize;
    for fact in &extension.facts {
        if fact.name.is_empty()
            || fact.name.len() > MAX_VENDOR_FACT_NAME_BYTES
            || fact.value.len() > MAX_VENDOR_FACT_VALUE_BYTES
            || !names.insert(fact.name.as_str())
        {
            return false;
        }
        total_bytes = total_bytes.saturating_add(fact.name.len() + fact.value.len());
    }
    total_bytes <= MAX_VENDOR_FACT_TOTAL_BYTES
}

#[test]
fn valid_inbound_message_preserves_mixed_content_reply_and_vendor_facts() {
    let inbound = inbound_fixture();
    let decoded = InboundMessagePayload::decode(inbound.encode_to_vec().as_slice()).unwrap();

    assert_eq!(decoded.message_id, "9002");
    assert_eq!(
        decoded.conversation.as_ref().unwrap().kind(),
        ConversationKind::Group
    );
    assert!(matches!(
        decoded.content[0].kind,
        Some(message_content_part::Kind::Mention(_))
    ));
    assert!(matches!(
        decoded.content[1].kind,
        Some(message_content_part::Kind::Text(_))
    ));
    assert!(matches!(
        decoded.content[2].kind,
        Some(message_content_part::Kind::Image(_))
    ));
    assert!(matches!(
        decoded.content[3].kind,
        Some(message_content_part::Kind::File(_))
    ));
    assert_eq!(decoded.reply.unwrap().message_id, "777");
    assert_eq!(decoded.vendor_extension.unwrap(), vendor_extension());
}

#[test]
fn typed_any_payloads_round_trip_through_capability_execution_contract() {
    let inbound = inbound_fixture();
    let event = CapabilityApplicationEvent {
        subscription_id: "subscription-1".to_string(),
        capability: CAPABILITY_ID.to_string(),
        event_sequence: 1,
        event_type: INBOUND_MESSAGE_EVENT_TYPE.to_string(),
        payload: Some(pack(INBOUND_MESSAGE_TYPE_URL, &inbound)),
        generation: 7,
        source_id: "onebot-connector".to_string(),
    };

    let unpacked: InboundMessagePayload =
        unpack(event.payload.as_ref().unwrap(), INBOUND_MESSAGE_TYPE_URL);
    assert_eq!(unpacked, inbound);

    let send = SendMessageRequest {
        conversation: Some(onebot_group_scope()),
        content: vec![MessageContentPart {
            kind: Some(message_content_part::Kind::Text(TextContent {
                text: "answer".to_string(),
            })),
        }],
        reply: Some(ReplyReference {
            message_id: "9002".to_string(),
        }),
        vendor_extension: None,
    };
    let invoke = InvokeCapabilityRequest {
        capability: CAPABILITY_ID.to_string(),
        interface_version: INTERFACE_VERSION.to_string(),
        method: SEND_MESSAGE_METHOD.to_string(),
        request: Some(pack(SEND_MESSAGE_REQUEST_TYPE_URL, &send)),
    };
    let unpacked_send: SendMessageRequest = unpack(
        invoke.request.as_ref().unwrap(),
        SEND_MESSAGE_REQUEST_TYPE_URL,
    );
    assert_eq!(unpacked_send, send);

    let result = DeliveryResult {
        status: DeliveryStatus::Accepted as i32,
        vendor_message_id: "vendor-9003".to_string(),
        reason: String::new(),
        retry_after: None,
        vendor_extension: None,
    };
    let response = InvokeCapabilityResponse {
        result: Some(invoke_capability_response::Result::Response(pack(
            DELIVERY_RESULT_TYPE_URL,
            &result,
        ))),
    };
    let invoke_capability_response::Result::Response(payload) = response.result.unwrap() else {
        panic!("delivery result must use the capability response branch");
    };
    let unpacked_result: DeliveryResult = unpack(&payload, DELIVERY_RESULT_TYPE_URL);
    assert_eq!(unpacked_result, result);
}

#[test]
fn delivery_results_are_truthful_connector_outcomes_only() {
    let outcomes = [
        DeliveryResult {
            status: DeliveryStatus::Accepted as i32,
            vendor_message_id: "message-1".to_string(),
            reason: String::new(),
            retry_after: None,
            vendor_extension: None,
        },
        DeliveryResult {
            status: DeliveryStatus::Rejected as i32,
            vendor_message_id: String::new(),
            reason: "vendor rejected request".to_string(),
            retry_after: None,
            vendor_extension: None,
        },
        DeliveryResult {
            status: DeliveryStatus::RateLimited as i32,
            vendor_message_id: String::new(),
            reason: "vendor rate limit".to_string(),
            retry_after: Some(Duration {
                seconds: 30,
                nanos: 0,
            }),
            vendor_extension: None,
        },
    ];

    assert_eq!(outcomes[0].status(), DeliveryStatus::Accepted);
    assert_eq!(outcomes[1].status(), DeliveryStatus::Rejected);
    assert_eq!(outcomes[2].status(), DeliveryStatus::RateLimited);
    assert_eq!(outcomes[2].retry_after.as_ref().unwrap().seconds, 30);
}

#[test]
fn vendor_extension_limits_are_deterministic_and_preserve_valid_facts() {
    let extension = vendor_extension();
    assert!(valid_vendor_extension(&extension));

    let mut too_many = extension.clone();
    too_many.facts = (0..=MAX_VENDOR_FACTS)
        .map(|index| VendorFact {
            name: format!("fact_{index}"),
            value: "value".to_string(),
        })
        .collect();
    assert!(!valid_vendor_extension(&too_many));

    let mut duplicate = extension.clone();
    duplicate.facts.push(duplicate.facts[0].clone());
    assert!(!valid_vendor_extension(&duplicate));

    let mut oversized = extension;
    oversized.facts[0].value = "x".repeat(MAX_VENDOR_FACT_VALUE_BYTES + 1);
    assert!(!valid_vendor_extension(&oversized));
}

#[test]
fn schema_has_one_reply_authority_no_service_and_no_product_policy_fields() {
    let schema = include_str!("../../../proto/cyrene/message/connector/v1/message_connector.proto");
    assert!(!schema
        .lines()
        .any(|line| line.trim_start().starts_with("service ")));
    assert!(!schema.contains("ReplyContent"));
    assert!(!schema.contains("reply_to_message_id"));
    assert!(!schema.contains("bytes payload"));
    assert!(!schema.contains("local_path"));
    assert!(!schema.contains("file_path"));

    let source_fields = schema
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    for forbidden_field in [
        "wake_word",
        "should_reply",
        "authorization_policy",
        "admin_policy",
        "persona",
        "session_mapping",
        "conversation_persistence",
        "memory_policy",
        "rag_policy",
        "model_routing",
        "tool_routing",
        "command_semantics",
        "iris",
        "product_lifecycle",
    ] {
        assert!(
            !source_fields.contains(forbidden_field),
            "Product policy field leaked into connector schema: {forbidden_field}"
        );
    }

    assert_eq!(CAPABILITY_ID, "message.connector.v1");
    assert_eq!(INTERFACE_VERSION, "1");
    assert_eq!(SEND_MESSAGE_METHOD, "send_message");
    assert_eq!(INBOUND_MESSAGE_EVENT_TYPE, "inbound_message");
}
