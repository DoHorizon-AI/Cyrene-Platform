#!/usr/bin/env bash
# Retired Platform V1 Relay transport acceptance entrypoint.
# 旧 Platform V1 Relay transport 验收入口。

set -euo pipefail
set +x

printf '%s\n' \
  'RETIRED: this runner exercised the removed Platform cy-workspace-relay-host and its legacy Tonic/XFCC path.' \
  'It does not run acceptance against the Plugins-owned Relay runtime or current Authority deployment.' \
  'Use Cyrene-Plugins-Official/.github/workflows/component-release.yml for package checks; those checks do not prove deployment or end-to-end authorization.' \
  '已退役：此脚本验证的是已移除的 Platform 旧 Relay host/Tonic/XFCC 路径，不验证当前 Plugins runtime 或 Authority 部署。' >&2
exit 2
