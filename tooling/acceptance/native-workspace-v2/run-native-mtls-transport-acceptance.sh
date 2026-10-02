#!/usr/bin/env bash
# Refresh the isolated signed CRL, exercise the real local Native Relay, then stop it.
# 在同一短时 CRL 窗口中刷新、启动、验证并关闭本机 Relay。

set -euo pipefail
set +x
umask 077

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
acceptance_root="/tmp/cyrene-components-v2-acceptance/native-relay"
relay_runner="${script_dir}/run-native-relay-local.sh"
relay_started=0

cleanup() {
  local result="$?"
  trap - EXIT INT TERM
  if [[ "${relay_started}" == '1' ]]; then
    if "${relay_runner}" stop >/dev/null 2>&1; then
      printf '%s\n' 'Native Relay stopped after the local transport probe.'
    else
      printf 'Native Relay cleanup needs attention; inspect private PID/log files under %s\n' \
        "${acceptance_root}" >&2
      result=1
    fi
  fi
  exit "${result}"
}
trap cleanup EXIT INT TERM

# Release a stale PID record left by an earlier failed or interrupted local run.
"${relay_runner}" stop >/dev/null

# Warm both compiled binaries before starting the five-minute signed-CRL window.
repo_root="$(cd "${script_dir}/../../.." && pwd)"
cd "${repo_root}"
CARGO_TARGET_DIR="/tmp/cyrene-components-target" cargo build --locked --offline \
  -p cy-workspace-relay-host \
  --bin cy-workspace-relay-host
CARGO_TARGET_DIR="${acceptance_root}/cargo-target" cargo build --locked --offline \
  --manifest-path "${script_dir}/native-mtls-xfcc-probe/Cargo.toml"

# The restricted signer writes a fresh CRL before Relay's reader validates it.
"${script_dir}/run-device-ca-signer-check.sh"
relay_started=1
"${relay_runner}" start
"${script_dir}/run-native-mtls-xfcc-negative.sh"

health_status="$(curl --silent --show-error --max-time 2 --output "${acceptance_root}/native-relay-health-after-probe.json" --write-out '%{http_code}' \
  http://127.0.0.1:18081/readyz)"
if [[ "${health_status}" != '200' ]]; then
  printf '%s\n' 'Native Relay lost readiness during the local transport probe.' >&2
  exit 1
fi
chmod 0600 "${acceptance_root}/native-relay-health-after-probe.json"

python3 - "${acceptance_root}" "${repo_root}" <<'PY'
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

acceptance_root = Path(sys.argv[1])
repo_root = Path(sys.argv[2])
tls_report = json.loads((acceptance_root / "native-mtls-negative-report.json").read_text())
tonic_report = json.loads((acceptance_root / "native-tonic-xfcc-negative-report.json").read_text())
ready_before = json.loads((acceptance_root / "native-relay-readiness.json").read_text())
health_report = json.loads((acceptance_root / "native-relay-health-after-probe.json").read_text())
head = subprocess.run(
    ["git", "rev-parse", "HEAD"], cwd=repo_root, check=True, capture_output=True, text=True
).stdout.strip()
report = {
    "status": "PASS",
    "category": "REAL_NATIVE_TONIC_MTLS_TRANSPORT_LOCAL_ONLY",
    "runAtUtc": datetime.now(timezone.utc).isoformat(),
    "platformHead": head,
    "relayReadyBeforeProbe": ready_before.get("status") == "ready",
    "relayReadyBeforeHttpStatus": 200,
    "relayReadyAfterProbe": health_report.get("status") == "ready",
    "relayReadyAfterHttpStatus": 200,
    "certificateTransportProbe": {
        "category": tls_report.get("category"),
        "status": tls_report.get("status"),
        "checks": tls_report.get("checks"),
        "report": str(acceptance_root / "native-mtls-negative-report.json"),
    },
    "tonicProbe": {
        "category": tonic_report.get("category"),
        "status": tonic_report.get("status"),
        "checks": tonic_report.get("checks"),
        "report": str(acceptance_root / "native-tonic-xfcc-negative-report.json"),
    },
    "scope": "real local Native Relay and Tonic mTLS workload transport; no AAD principal, device leaf, human approval, ACK, or Product dispatch",
    "sourceFiles": [
        "framework/crates/cy-workspace-relay-host/src/main.rs",
        "framework/crates/cy-workspace-postgres-storage/src/restricted_device_ca.rs",
        "tooling/acceptance/native-workspace-v2/probe-native-mtls-negative.py",
        "tooling/acceptance/native-workspace-v2/native-mtls-xfcc-probe/src/main.rs",
    ],
}
if (
    tls_report.get("status") != "PASS"
    or tonic_report.get("status") != "PASS"
    or not report["relayReadyBeforeProbe"]
    or not report["relayReadyAfterProbe"]
):
    raise SystemExit("Local Native Relay transport acceptance reports are incomplete.")
target = acceptance_root / "native-mtls-transport-acceptance-report.json"
target.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
target.chmod(0o600)
print(f"REAL_NATIVE_TONIC_MTLS_TRANSPORT_LOCAL_ONLY PASS: report={target}")
PY
