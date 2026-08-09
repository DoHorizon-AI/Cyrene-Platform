# CYRENE Plugin Wire Protocol Specification

- **Protocol Version:** `1`
- **Status:** Normative
- **Target Protobuf Package:** `cy.plugin.v1`

---

## 1. Overview

The CYRENE Plugin Protocol defines the binary messaging format exchanged between the Rust Host (`PluginSupervisor`) and out-of-process plugins (Python, JVM, or native binaries) operating over zero-port local channels (`stdio`, UDS, or Named Pipes).

---

## 2. Binary Framing Specification

Messages exchanged over `stdio` streams use a fixed 4-byte Length-Prefixed framing:

```text
+-------------------+--------------------------------------------------+
| Length (4 bytes)  | Payload (Length bytes)                           |
| Big-Endian uint32 | Encoded protobuf `cy.plugin.v1.Envelope` message |
+-------------------+--------------------------------------------------+
```

### Framing Rules
1. **Length Header:** A 32-bit unsigned integer in network byte order (Big-Endian) indicating the size in bytes of the following Protobuf `Envelope` message.
2. **Maximum Message Size:** Default upper limit is `67,108,864` bytes (64 MiB). Frames declaring a length greater than the maximum size MUST be rejected with a `ProtocolError`, triggering stream closure.
3. **Stream Boundaries:**
   - Writing to `stdin`/`stdout` MUST be serialized per channel.
   - Half-reads and TCP/stdio chunking MUST be handled by buffering until `Length` bytes are accumulated.

---

## 3. Protocol Message Structure (Protobuf)

All framing payloads deserialize into a top-level `cy.plugin.v1.Envelope` containing generic headers and an explicit message payload.

### 3.1 `Envelope` Message

```protobuf
syntax = "proto3";

package cy.plugin.v1;

message Envelope {
  string request_id = 1;        // Unique correlation UUID per request
  string trace_id = 2;          // Tracing context ID
  string plugin_id = 3;         // Origin/Target plugin unique ID
  uint32 protocol_version = 4;  // Wire protocol version (must be 1)
  int64 deadline_ms = 5;        // Unix epoch timestamp in milliseconds when request expires
  uint64 sequence_number = 6;   // Sequence number for streaming items (0-indexed)

  oneof payload {
    // Control / Lifecycle Messages
    Hello hello = 10;
    HelloAck hello_ack = 11;
    Configure configure = 12;
    Cancel cancel = 13;
    HealthCheck health_check = 14;
    HealthStatus health_status = 15;
    Shutdown shutdown = 16;

    // RPC & Extension Point Messages
    Invoke invoke = 20;
    InvokeResult invoke_result = 21;
    StreamItem stream_item = 22;

    // Error Message
    PluginErrorPayload error = 30;
  }
}
```

---

## 4. Control Messages

### 4.1 Handshake (`Hello` & `HelloAck`)
Immediately after launch, the host sends `Hello`. The plugin MUST respond with `HelloAck` before any `Invoke` requests can be processed.

```protobuf
message Hello {
  uint32 min_protocol_version = 1;
  uint32 max_protocol_version = 2;
  string host_version = 3;
}

message HelloAck {
  uint32 selected_protocol_version = 1;
  string plugin_id = 2;
  string plugin_version = 3;
  string api_version = 4;
  repeated string declared_capabilities = 5;
}
```

### 4.2 Health & Cancellation

```protobuf
message Cancel {
  string target_request_id = 1;
  string reason = 2;
}

message HealthCheck {}

message HealthStatus {
  enum Status {
    HEALTHY = 0;
    DEGRADED = 1;
    UNHEALTHY = 2;
  }
  Status status = 1;
  string message = 2;
}

message Shutdown {
  uint32 grace_period_ms = 1;
}
```

---

## 5. Error Classification (`PluginErrorPayload`)

```protobuf
message PluginErrorPayload {
  enum Code {
    UNKNOWN = 0;
    UNAVAILABLE = 1;
    INCOMPATIBLE = 2;
    INVALID_INPUT = 3;
    PERMISSION_DENIED = 4;
    TIMEOUT = 5;
    CANCELLED = 6;
    RETRYABLE = 7;
    EXECUTION_FAILED = 8;
    PROTOCOL_ERROR = 9;
    FATAL = 10;
  }
  Code code = 1;
  string message = 2;
  string details = 3;
}
```

---

## 6. Zero-Port Local Transport Rules

- **Default:** `StdioTransport` (Host `stdin` <-> Plugin `stdout`).
- **Standard Error (`stderr`):** Retained purely for OS-level logging, stderr output is parsed line-by-line by `PluginSupervisor` and forwarded to host logging facilities.
- **Port Prohibition:** Local out-of-process plugins MUST NOT open TCP server sockets or listen on network interfaces for IPC.
