#!/bin/sh
# Product-neutral long-running fixture with observable SIGTERM handling.
# 产品无关长任务 fixture：记录 SIGTERM 是否完成优雅退出。

set -eu

state_dir=$1
generation=${2:?runtime generation is required}
printf '%s\n' "running" > "${state_dir}/workload-running-${generation}"
printf '%s\n' "fixture workload generation ${generation} started"
printf '%s\n' "CYRENE_PROGRESS 1/10 steps"

graceful_exit() {
    printf '%s\n' "fixture workload generation ${generation} stopping"
    printf '%s\n' "CYRENE_PROGRESS 10/10 steps"
    printf '%s\n' "graceful" > "${state_dir}/workload-graceful-${generation}"
    exit 0
}

trap graceful_exit TERM INT
while :; do
    sleep 1
done
