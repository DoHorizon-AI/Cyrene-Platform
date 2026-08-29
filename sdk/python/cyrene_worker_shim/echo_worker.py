#!/usr/bin/env python3
"""Example Echo Worker implemented using cyrene_worker_shim."""

import sys
from typing import Dict, List, Optional, Tuple

from cyrene_worker import CyreneWorker, run_worker_stdio


class EchoWorker(CyreneWorker):
    def plugin_id(self) -> str:
        return "com.cyrene.test.python-echo-worker"

    def plugin_version(self) -> str:
        return "1.0.0"

    def api_version(self) -> str:
        return "1.0"

    def declared_capabilities(self) -> List[str]:
        return ["Echo", "PythonShim"]

    def on_invoke(self, capability: str, action: str, payload: bytes) -> Tuple[bool, bytes]:
        # Echo back the payload
        return True, payload


if __name__ == "__main__":
    run_worker_stdio(EchoWorker())
