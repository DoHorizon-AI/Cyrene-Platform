#!/usr/bin/env bash
# Retired Platform V1 Relay mTLS/XFCC probe entrypoint.
# 旧 Platform V1 Relay mTLS/XFCC 探针入口。

set -euo pipefail
set +x

printf '%s\n' \
  'RETIRED: the XFCC rejection probe targets the removed Platform Relay protocol.' \
  'The Plugins-owned Relay has a different interface; use its package checks in Cyrene-Plugins-Official/.github/workflows/component-release.yml.' \
  'Package checks do not establish deployed transport, Authority, device-approval, or Product-dispatch acceptance.' \
  '已退役：XFCC 探针针对的 Platform Relay 协议已移除；Plugins Relay 接口不同。' >&2
exit 2
