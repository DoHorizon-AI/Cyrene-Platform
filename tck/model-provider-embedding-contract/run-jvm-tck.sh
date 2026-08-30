#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "${script_dir}/../.." && pwd -P)"
wrapper="${repo_root}/framework/jvm/gradlew"
wrapper_jar="${repo_root}/framework/jvm/gradle/wrapper/gradle-wrapper.jar"

if [[ -f "${wrapper_jar}" ]]; then
    bash "${wrapper}" -p "${script_dir}/jvm" test "$@"
elif command -v gradle >/dev/null 2>&1; then
    gradle -p "${script_dir}/jvm" test "$@"
else
    gradle_cache_root="${GRADLE_USER_HOME:-${HOME}/.gradle}"
    gradle_binary="$(find "${gradle_cache_root}/wrapper/dists" -type f -path '*/bin/gradle' -print 2>/dev/null | sort -V | tail -n 1)"
    if [[ -z "${gradle_binary}" ]]; then
        echo "Gradle is unavailable; install Gradle or restore the repository wrapper jar." >&2
        exit 1
    fi
    "${gradle_binary}" -p "${script_dir}/jvm" test "$@"
fi
