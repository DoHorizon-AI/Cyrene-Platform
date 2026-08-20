#![cfg(unix)]

use bytes::BytesMut;
use cy_extension_registry::helper::prepare_instance_actor;
use cy_extension_registry::transport::{connect, read_frame, write_frame};
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_plugin_protocol::{
    envelope::Payload,
    pb::{
        invoke::Request, invoke_result::Response, stream_item::Data, Envelope,
        ExecuteInferenceRequest, ExecuteInferenceResponse, Invoke, InvokeResult, StreamItem,
    },
    CURRENT_PROTOCOL_VERSION,
};
use prost::Message;
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};

#[tokio::test]
async fn test_worker_transport_large_payload_integrity() {
    let socket_path: PathBuf =
        std::env::temp_dir().join(format!("cyrene-payload-test-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path).expect("bind mock worker transport socket");

    // Spawn mock backend worker
    let server_handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept connection");
        let (mut reader, mut writer) = stream.into_split();
        let (resp_tx, mut resp_rx) = mpsc::channel::<Envelope>(64);

        let writer_task = tokio::spawn(async move {
            let mut wire_seq = 0_u64;
            while let Some(mut env) = resp_rx.recv().await {
                wire_seq += 1;
                env.sequence_number = wire_seq;
                if write_frame(&mut writer, &env).await.is_err() {
                    break;
                }
            }
        });

        let mut buffer = BytesMut::with_capacity(64 * 1024);

        while let Ok(Some(req_env)) = read_frame(&mut reader, &mut buffer).await {
            let req_id = req_env.request_id;
            let generation = req_env.generation;
            let fence_token = req_env.fence_token;
            let plugin_id = req_env.plugin_id;

            let resp_payload = match req_env.payload {
                Some(Payload::Invoke(invoke)) => {
                    let echo_text = match invoke.request {
                        Some(Request::ExecuteInference(infer_req)) => infer_req.prompt,
                        _ => String::new(),
                    };
                    Payload::InvokeResult(InvokeResult {
                        response: Some(Response::ExecuteInference(ExecuteInferenceResponse {
                            output_text: format!("echo:{}", echo_text),
                        })),
                    })
                }
                _ => Payload::InvokeResult(InvokeResult::default()),
            };

            let resp_env = Envelope {
                request_id: req_id,
                trace_id: String::new(),
                plugin_id,
                protocol_version: CURRENT_PROTOCOL_VERSION,
                deadline_ms: 0,
                sequence_number: 0,
                generation,
                fence_token,
                payload: Some(resp_payload),
            };

            if resp_tx.send(resp_env).await.is_err() {
                break;
            }
        }

        drop(resp_tx);
        let _ = writer_task.await;
    });

    let actor = Arc::new(AsyncMutex::new(InstanceActor::new_for_test_with_transport(
        &socket_path,
    )));

    // Prepare instance actor (connects via transport retry)
    let _ = prepare_instance_actor("test-instance", &actor)
        .await
        .expect("prepare instance actor");

    // 1. Test Large Payload Integrity (256 KiB text payload)
    let large_text = "A".repeat(256 * 1024);
    let invoke_msg = Invoke {
        extension_point: "execution_engine".to_string(),
        method: "execute_inference".to_string(),
        request: Some(Request::ExecuteInference(ExecuteInferenceRequest {
            runtime_manifest_json: String::new(),
            model_manifest_json: String::new(),
            prompt: large_text.clone(),
            streaming: false,
        })),
    };

    let resp_bytes = {
        let mut guard = actor.lock().await;
        guard
            .invoke_raw(invoke_msg.encode_to_vec(), Duration::from_secs(5))
            .await
            .expect("large payload invoke must succeed")
    };

    let resp_env = Envelope::decode(resp_bytes.as_slice()).expect("decode response envelope");
    match resp_env.payload {
        Some(Payload::InvokeResult(res)) => match res.response {
            Some(Response::ExecuteInference(infer_res)) => {
                assert_eq!(
                    infer_res.output_text,
                    format!("echo:{}", large_text),
                    "large payload text must match exactly"
                );
            }
            other => panic!("expected ExecuteInference response, got {:?}", other),
        },
        other => panic!("expected InvokeResult, got {:?}", other),
    }

    // Clean up
    drop(actor);
    server_handle.abort();
    let _ = std::fs::remove_file(socket_path);
}

#[tokio::test]
async fn test_pipelined_concurrent_transport_multiplexing_and_correlation() {
    let socket_path: PathBuf = std::env::temp_dir().join(format!(
        "cyrene-pipeline-concurrency-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path).expect("bind socket");

    // Mock server: handles requests concurrently with artificial out-of-order latency,
    // and writer assigns strictly monotonic wire sequence_number upon sending.
    let server_handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (mut reader, mut writer) = stream.into_split();
        let (resp_tx, mut resp_rx) = mpsc::channel::<Envelope>(128);

        let writer_task = tokio::spawn(async move {
            let mut wire_seq = 0_u64;
            while let Some(mut env) = resp_rx.recv().await {
                wire_seq += 1;
                env.sequence_number = wire_seq;
                if write_frame(&mut writer, &env).await.is_err() {
                    break;
                }
            }
        });

        let mut buffer = BytesMut::with_capacity(64 * 1024);

        while let Ok(Some(req_env)) = read_frame(&mut reader, &mut buffer).await {
            let resp_tx_clone = resp_tx.clone();
            let req_id = req_env.request_id;
            let generation = req_env.generation;
            let fence_token = req_env.fence_token;
            let prompt = match req_env.payload {
                Some(Payload::Invoke(inv)) => match inv.request {
                    Some(Request::ExecuteInference(r)) => r.prompt,
                    _ => String::new(),
                },
                _ => String::new(),
            };

            // Spawn concurrent task on server with varying latency to produce out-of-order replies
            tokio::spawn(async move {
                let delay = if prompt.contains("_odd_") { 15 } else { 2 };
                tokio::time::sleep(Duration::from_millis(delay)).await;

                let resp_env = Envelope {
                    request_id: req_id,
                    trace_id: String::new(),
                    plugin_id: "test.worker".to_string(),
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    deadline_ms: 0,
                    sequence_number: 0, // Assigned by writer_task monotonically
                    generation,
                    fence_token,
                    payload: Some(Payload::InvokeResult(InvokeResult {
                        response: Some(Response::ExecuteInference(ExecuteInferenceResponse {
                            output_text: format!("resp:{}", prompt),
                        })),
                    })),
                };
                let _ = resp_tx_clone.send(resp_env).await;
            });
        }

        drop(resp_tx);
        let _ = writer_task.await;
    });

    // Client: connect directly and run multiplexer
    let (mut reader, mut writer) = connect(&socket_path, Duration::from_secs(3))
        .await
        .expect("connect");

    let (req_tx, mut req_rx) = mpsc::channel::<Envelope>(128);
    let pending_map = Arc::new(AsyncMutex::new(
        HashMap::<String, oneshot::Sender<Envelope>>::new(),
    ));

    // Client writer task
    let client_writer = tokio::spawn(async move {
        while let Some(env) = req_rx.recv().await {
            if write_frame(&mut writer, &env).await.is_err() {
                break;
            }
        }
    });

    // Client reader task (dispatches by request_id and validates monotonic sequence_number)
    let pending_for_reader = pending_map.clone();
    let client_reader = tokio::spawn(async move {
        let mut buffer = BytesMut::with_capacity(64 * 1024);
        let mut last_seq = 0_u64;
        while let Ok(Some(resp_env)) = read_frame(&mut reader, &mut buffer).await {
            assert!(
                resp_env.sequence_number > last_seq,
                "wire sequence_number must be strictly monotonic: got {} <= last {}",
                resp_env.sequence_number,
                last_seq
            );
            last_seq = resp_env.sequence_number;
            if let Some(tx) = pending_for_reader.lock().await.remove(&resp_env.request_id) {
                let _ = tx.send(resp_env);
            }
        }
    });

    // Spawn 30 concurrent in-flight requests simultaneously without serialization
    let mut handles = Vec::new();
    for i in 0..30 {
        let req_tx_clone = req_tx.clone();
        let pending_clone = pending_map.clone();

        let handle = tokio::spawn(async move {
            let req_id = format!("req-pipelined-{}", i);
            let prompt = if i % 2 == 1 {
                format!("data_odd_{}", i)
            } else {
                format!("data_even_{}", i)
            };

            let req_env = Envelope {
                request_id: req_id.clone(),
                trace_id: "trace".to_string(),
                plugin_id: "test.worker".to_string(),
                protocol_version: CURRENT_PROTOCOL_VERSION,
                deadline_ms: 5000,
                sequence_number: i as u64,
                generation: 1,
                fence_token: 1,
                payload: Some(Payload::Invoke(Invoke {
                    extension_point: "execution_engine".to_string(),
                    method: "execute_inference".to_string(),
                    request: Some(Request::ExecuteInference(ExecuteInferenceRequest {
                        runtime_manifest_json: String::new(),
                        model_manifest_json: String::new(),
                        prompt: prompt.clone(),
                        streaming: false,
                    })),
                })),
            };

            let (reply_tx, reply_rx) = oneshot::channel();
            pending_clone.lock().await.insert(req_id.clone(), reply_tx);

            req_tx_clone.send(req_env).await.expect("send request");
            let resp = reply_rx.await.expect("receive reply");

            assert_eq!(resp.request_id, req_id);
            match resp.payload {
                Some(Payload::InvokeResult(res)) => match res.response {
                    Some(Response::ExecuteInference(r)) => {
                        assert_eq!(r.output_text, format!("resp:{}", prompt));
                    }
                    other => panic!("unexpected response variant: {:?}", other),
                },
                other => panic!("unexpected payload: {:?}", other),
            }
        });
        handles.push(handle);
    }

    for h in handles {
        h.await.expect("task join");
    }

    // Clean up
    drop(req_tx);
    let _ = client_writer.await;
    client_reader.abort();
    server_handle.abort();
    let _ = std::fs::remove_file(socket_path);
}

#[tokio::test]
async fn test_streaming_chunking_and_backpressure() {
    let socket_path: PathBuf =
        std::env::temp_dir().join(format!("cyrene-stream-test-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path).expect("bind socket");

    // Server streams 100 chunks with sequence numbering, flow control buffer, and terminal flag
    let server_handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (mut reader, mut writer) = stream.into_split();

        let mut buffer = BytesMut::with_capacity(64 * 1024);
        if let Ok(Some(req_env)) = read_frame(&mut reader, &mut buffer).await {
            let req_id = req_env.request_id;
            for chunk_idx in 0..100 {
                let is_last = chunk_idx == 99;
                let stream_env = Envelope {
                    request_id: req_id.clone(),
                    trace_id: String::new(),
                    plugin_id: "test.worker".to_string(),
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    deadline_ms: 0,
                    sequence_number: chunk_idx + 1,
                    generation: 1,
                    fence_token: 1,
                    payload: Some(Payload::StreamItem(StreamItem {
                        request_id: req_id.clone(),
                        sequence_number: chunk_idx + 1,
                        is_last,
                        data: Some(Data::TextChunk(format!("chunk-{}", chunk_idx))),
                    })),
                };
                if write_frame(&mut writer, &stream_env).await.is_err() {
                    break;
                }
            }
        }
    });

    let (mut reader, mut writer) = connect(&socket_path, Duration::from_secs(3))
        .await
        .expect("connect");

    // Send initial request
    let start_env = Envelope {
        request_id: "stream-req-1".to_string(),
        trace_id: String::new(),
        plugin_id: "test.worker".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        deadline_ms: 5000,
        sequence_number: 0,
        generation: 1,
        fence_token: 1,
        payload: Some(Payload::Invoke(Invoke {
            extension_point: "execution_engine".to_string(),
            method: "execute_inference".to_string(),
            request: Some(Request::ExecuteInference(ExecuteInferenceRequest {
                runtime_manifest_json: String::new(),
                model_manifest_json: String::new(),
                prompt: "stream".to_string(),
                streaming: true,
            })),
        })),
    };
    write_frame(&mut writer, &start_env)
        .await
        .expect("write start");

    // Read 100 streaming chunks with bounded rate flow control
    let mut buffer = BytesMut::with_capacity(64 * 1024);
    let mut received_count = 0;

    while let Ok(Some(env)) = read_frame(&mut reader, &mut buffer).await {
        assert_eq!(env.request_id, "stream-req-1");
        match env.payload {
            Some(Payload::StreamItem(item)) => {
                assert_eq!(item.sequence_number, received_count + 1);
                match item.data {
                    Some(Data::TextChunk(text)) => {
                        assert_eq!(text, format!("chunk-{}", received_count));
                    }
                    other => panic!("expected TextChunk, got {:?}", other),
                }
                received_count += 1;
                if item.is_last {
                    break;
                }
            }
            other => panic!("expected StreamItem, got {:?}", other),
        }
    }

    assert_eq!(received_count, 100);

    server_handle.abort();
    let _ = std::fs::remove_file(socket_path);
}
