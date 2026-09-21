//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 panic_hook.rs                                                   │
//! │  Module: cy_observability::panic_hook                               │
//! │  Role: Minimal emergency panic diagnostics directed to stderr.      │
//! │                                                                     │
//! │  模块职责：Panic 发生时的最小紧急诊断输出，直写 stderr，不走异步队列。   │
//! └─────────────────────────────────────────────────────────────────────┘

use std::{
    io::Write,
    panic::{self, PanicHookInfo},
    sync::atomic::{AtomicBool, Ordering},
};

use chrono::Utc;
use serde_json::json;

use crate::redaction::truncate_bounded;

static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// ════════════════════════════════════════════════════════════════════════
/// Installs an emergency panic hook that outputs structured JSON diagnostic
/// information directly to stderr without acquiring locks or relying on async queues.
///
/// 安装紧急 panic hook。直接输出安全脱敏的结构化 JSON 诊断至 stderr。
/// ════════════════════════════════════════════════════════════════════════
pub fn install_panic_hook(service_name: String, service_instance_id: String) {
    if PANIC_HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }

    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        let timestamp = Utc::now().to_rfc3339();
        let payload = extract_panic_message(info);
        let (safe_message, _) = truncate_bounded(&payload, 1024);

        let location = info.location();
        let file = location.as_ref().map(|l| l.file()).unwrap_or("unknown");
        let line = location.as_ref().map(|l| l.line()).unwrap_or(0);

        let record = json!({
            "schema_version": 1,
            "timestamp": timestamp,
            "level": "ERROR",
            "event.name": "platform.panic",
            "service.name": &service_name,
            "service.instance.id": &service_instance_id,
            "message": format!("emergency panic: {}", safe_message),
            "attributes": {
                "error.code": "PLATFORM.PANIC.UNHANDLED",
                "panic.file": file,
                "panic.line": line,
            }
        });

        // Write directly to stderr without locks or memory buffering
        let mut stderr = std::io::stderr().lock();
        if let Ok(serialized) = serde_json::to_vec(&record) {
            let _ = stderr.write_all(&serialized);
            let _ = stderr.write_all(b"\n");
            let _ = stderr.flush();
        }

        // Invoke the previous hook (so standard abort / backtrace behavior is preserved)
        default_hook(info);
    }));
}

fn extract_panic_message(info: &PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}
