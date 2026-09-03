use cy_proto::{
    capability_v1::{
        invoke_capability_response, CapabilityExecutionError, InvokeCapabilityRequest,
        InvokeCapabilityResponse,
    },
    model_provider::{
        CAPABILITY_ID, EMBEDDINGS_METHOD, EMBEDDINGS_REQUEST_TYPE_URL,
        EMBEDDINGS_RESPONSE_TYPE_URL, INTERFACE_VERSION,
    },
    model_provider_v1::{
        embedding_error, embeddings_response, EmbeddingBatch, EmbeddingError, EmbeddingVector,
        EmbeddingsRequest, EmbeddingsResponse,
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
    M::decode(payload.value.as_slice()).unwrap()
}

fn fake_provider(request: &EmbeddingsRequest, model: &str) -> EmbeddingsResponse {
    if request.inputs.is_empty() || request.inputs.iter().any(String::is_empty) {
        return domain_error(
            embedding_error::Code::InvalidInput,
            "inputs must be non-empty",
        );
    }
    if request.inputs.len() > 4 {
        return domain_error(
            embedding_error::Code::InvalidInput,
            "provider batch limit is four",
        );
    }
    if request.model.as_deref() == Some("missing") {
        return domain_error(
            embedding_error::Code::ModelNotAvailable,
            "requested model is unavailable",
        );
    }

    let vectors = request
        .inputs
        .iter()
        .enumerate()
        .map(|(index, input)| EmbeddingVector {
            values: vec![index as f32, input.len() as f32, 1.0],
        })
        .collect();
    EmbeddingsResponse {
        result: Some(embeddings_response::Result::Embeddings(EmbeddingBatch {
            vectors,
            dimensions: 3,
            model: request.model.clone().unwrap_or_else(|| model.to_string()),
        })),
    }
}

fn domain_error(code: embedding_error::Code, message: &str) -> EmbeddingsResponse {
    EmbeddingsResponse {
        result: Some(embeddings_response::Result::Error(EmbeddingError {
            code: code as i32,
            message: message.to_string(),
            retry_after: None,
        })),
    }
}

fn embeddings(response: EmbeddingsResponse) -> EmbeddingBatch {
    let Some(embeddings_response::Result::Embeddings(batch)) = response.result else {
        panic!("expected embedding batch");
    };
    batch
}

fn validate_batch(
    request: &EmbeddingsRequest,
    batch: &EmbeddingBatch,
) -> Result<(), embedding_error::Code> {
    let dimensions = batch.dimensions as usize;
    if dimensions == 0
        || batch.vectors.len() != request.inputs.len()
        || batch
            .vectors
            .iter()
            .any(|vector| vector.values.len() != dimensions)
    {
        return Err(embedding_error::Code::DimensionMismatch);
    }
    Ok(())
}

#[test]
fn single_and_ordered_batch_preserve_one_vector_per_input() {
    let single = embeddings(fake_provider(
        &EmbeddingsRequest {
            inputs: vec!["alpha".to_string()],
            model: None,
        },
        "configured-model",
    ));
    assert_eq!(single.vectors.len(), 1);
    assert_eq!(single.vectors[0].values, [0.0, 5.0, 1.0]);
    assert_eq!(single.dimensions, 3);
    assert_eq!(single.model, "configured-model");

    let batch = embeddings(fake_provider(
        &EmbeddingsRequest {
            inputs: vec!["a".to_string(), "bbbb".to_string(), "cc".to_string()],
            model: Some("selected-model".to_string()),
        },
        "configured-model",
    ));
    assert_eq!(batch.vectors.len(), 3);
    assert_eq!(batch.vectors[0].values, [0.0, 1.0, 1.0]);
    assert_eq!(batch.vectors[1].values, [1.0, 4.0, 1.0]);
    assert_eq!(batch.vectors[2].values, [2.0, 2.0, 1.0]);
    assert!(batch
        .vectors
        .iter()
        .all(|vector| vector.values.len() == batch.dimensions as usize));
    assert_eq!(batch.model, "selected-model");
    assert_eq!(
        validate_batch(
            &EmbeddingsRequest {
                inputs: vec!["a".to_string(), "bbbb".to_string(), "cc".to_string()],
                model: Some("selected-model".to_string()),
            },
            &batch,
        ),
        Ok(())
    );
}

#[test]
fn empty_request_empty_string_and_provider_batch_limit_are_atomic_errors() {
    for request in [
        EmbeddingsRequest {
            inputs: vec![],
            model: None,
        },
        EmbeddingsRequest {
            inputs: vec![String::new()],
            model: None,
        },
        EmbeddingsRequest {
            inputs: (0..5).map(|index| index.to_string()).collect(),
            model: None,
        },
    ] {
        let response = fake_provider(&request, "configured-model");
        let Some(embeddings_response::Result::Error(error)) = response.result else {
            panic!("invalid request must fail as one atomic batch");
        };
        assert_eq!(error.code(), embedding_error::Code::InvalidInput);
    }

    let whitespace = embeddings(fake_provider(
        &EmbeddingsRequest {
            inputs: vec!["   ".to_string()],
            model: None,
        },
        "configured-model",
    ));
    assert_eq!(whitespace.vectors[0].values[1], 3.0);
}

#[test]
fn all_domain_error_codes_are_distinct_and_wire_stable() {
    let codes = [
        embedding_error::Code::InvalidInput,
        embedding_error::Code::ModelNotAvailable,
        embedding_error::Code::DimensionMismatch,
        embedding_error::Code::ProviderError,
        embedding_error::Code::RateLimited,
    ];
    let encoded: Vec<i32> = codes.iter().map(|code| *code as i32).collect();
    assert_eq!(encoded, [1, 2, 3, 4, 5]);

    let missing = fake_provider(
        &EmbeddingsRequest {
            inputs: vec!["value".to_string()],
            model: Some("missing".to_string()),
        },
        "configured-model",
    );
    let Some(embeddings_response::Result::Error(error)) = missing.result else {
        panic!("missing model must be a domain error");
    };
    assert_eq!(error.code(), embedding_error::Code::ModelNotAvailable);

    let malformed = EmbeddingBatch {
        vectors: vec![EmbeddingVector {
            values: vec![1.0, 2.0],
        }],
        dimensions: 3,
        model: "broken-provider".to_string(),
    };
    assert_eq!(
        validate_batch(
            &EmbeddingsRequest {
                inputs: vec!["value".to_string()],
                model: None,
            },
            &malformed,
        ),
        Err(embedding_error::Code::DimensionMismatch)
    );

    for code in [
        embedding_error::Code::ProviderError,
        embedding_error::Code::RateLimited,
    ] {
        let response = domain_error(code, "deterministic provider failure");
        let Some(embeddings_response::Result::Error(error)) = response.result else {
            panic!("provider failure must be typed");
        };
        assert_eq!(error.code(), code);
    }
}

#[test]
fn typed_any_round_trip_and_binding_target_are_canonical() {
    let request_payload = EmbeddingsRequest {
        inputs: vec!["alpha".to_string(), "beta".to_string()],
        model: None,
    };
    let invoke = InvokeCapabilityRequest {
        capability: CAPABILITY_ID.to_string(),
        interface_version: INTERFACE_VERSION.to_string(),
        method: EMBEDDINGS_METHOD.to_string(),
        request: Some(pack(EMBEDDINGS_REQUEST_TYPE_URL, &request_payload)),
        binding_id: Some("openai-main".to_string()),
    };
    assert_eq!(
        unpack::<EmbeddingsRequest>(
            invoke.request.as_ref().unwrap(),
            EMBEDDINGS_REQUEST_TYPE_URL
        ),
        request_payload
    );
    assert_eq!(invoke.binding_id.as_deref(), Some("openai-main"));

    let typed = fake_provider(&request_payload, "configured-model");
    let response = InvokeCapabilityResponse {
        result: Some(invoke_capability_response::Result::Response(pack(
            EMBEDDINGS_RESPONSE_TYPE_URL,
            &typed,
        ))),
    };
    let invoke_capability_response::Result::Response(payload) = response.result.unwrap() else {
        panic!("embedding domain result must use the typed response branch");
    };
    assert_eq!(
        unpack::<EmbeddingsResponse>(&payload, EMBEDDINGS_RESPONSE_TYPE_URL),
        typed
    );
}

#[test]
fn timeout_and_cancelled_remain_generic_ces_execution_errors() {
    let timeout = CapabilityExecutionError {
        code: cy_proto::capability_v1::capability_execution_error::Code::Timeout as i32,
        message: "deadline expired".to_string(),
    };
    let cancelled = CapabilityExecutionError {
        code: cy_proto::capability_v1::capability_execution_error::Code::Cancelled as i32,
        message: "caller cancelled".to_string(),
    };
    assert_ne!(timeout.code, cancelled.code);
    assert_eq!(timeout.code, 7);
    assert_eq!(cancelled.code, 8);
}
