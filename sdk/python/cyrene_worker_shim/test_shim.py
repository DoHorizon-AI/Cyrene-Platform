import io
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))

from cyrene_worker import (
    Cancel, CancelAck, CyreneWorker, Envelope, Hello, HealthCheck, Shutdown, Invoke,
    run_worker_stream, write_frame, read_frame
)

class MyWorker(CyreneWorker):
    def plugin_id(self) -> str:
        return 'com.cy.analyzer'

    def plugin_version(self) -> str:
        return '1.2.3'

    def declared_capabilities(self):
        return ['ModelAnalyzer']

    def on_invoke(self, capability: str, action: str, payload: bytes):
        return True, b'ANALYSIS_OK:' + payload

    def on_cancel(self, target_request_id: str, reason: str) -> None:
        self.cancelled = (target_request_id, reason)

    def on_fence_rotated(self, new_generation: int, new_fence_token: int) -> None:
        self.rotated_events.append((new_generation, new_fence_token))

def main():
    worker = MyWorker()
    worker.cancelled = None
    worker.rotated_events = []
    inp = io.BytesIO()
    out = io.BytesIO()

    # 1. Hello with generation 2, fence 5
    e1 = Envelope(request_id='req-1', plugin_id='com.cy.analyzer', generation=2, fence_token=5, payload=Hello(min_protocol_version=1, max_protocol_version=1))
    write_frame(e1.encode(), inp)

    # 2. Advance generation to 3, fence 6 -> triggers on_fence_rotated
    e2 = Envelope(request_id='req-2', plugin_id='com.cy.analyzer', generation=3, fence_token=6, payload=HealthCheck())
    write_frame(e2.encode(), inp)

    # 3. Stale request with generation 1 -> rejected with FENCED_OUT
    e3 = Envelope(request_id='req-stale', plugin_id='com.cy.analyzer', generation=1, fence_token=5, payload=HealthCheck())
    write_frame(e3.encode(), inp)

    # 4. Invoke with active generation 3, fence 6
    e4 = Envelope(request_id='req-3', plugin_id='com.cy.analyzer', generation=3, fence_token=6, payload=Invoke(capability='ModelAnalyzer', action='Inspect', payload=b'data123'))
    write_frame(e4.encode(), inp)

    # 5. Cancel is acknowledged separately from the eventual Operation result.
    e5 = Envelope(request_id='cancel-1', plugin_id='com.cy.analyzer', generation=3, fence_token=6, payload=Cancel(target_request_id='req-3', reason='deadline'))
    write_frame(e5.encode(), inp)

    # 6. Shutdown
    e6 = Envelope(request_id='req-4', plugin_id='com.cy.analyzer', generation=3, fence_token=6, payload=Shutdown(grace_period_ms=500))
    write_frame(e6.encode(), inp)

    inp.seek(0)
    run_worker_stream(inp, out, worker)

    out.seek(0)

    # Decode outputs
    f1 = read_frame(out)
    assert f1 is not None
    r1 = Envelope.decode(f1)
    assert r1.request_id == 'req-1'
    assert r1.payload_tag == 11, f'expected HelloAck(11), got {r1.payload_tag}'
    assert r1.payload.plugin_id == 'com.cy.analyzer'
    assert r1.payload.declared_capabilities == ['ModelAnalyzer']

    f2 = read_frame(out)
    assert f2 is not None
    r2 = Envelope.decode(f2)
    assert r2.request_id == 'req-2'
    assert r2.payload_tag == 15, f'expected HealthStatus(15), got {r2.payload_tag}'
    assert r2.payload.status == 0

    # Verify on_fence_rotated was called for advance to (3, 6)
    assert worker.rotated_events == [(3, 6)], f'expected [(3, 6)], got {worker.rotated_events}'

    # Verify stale request was rejected with FENCED_OUT
    f3_stale = read_frame(out)
    assert f3_stale is not None
    r3_stale = Envelope.decode(f3_stale)
    assert r3_stale.request_id == 'req-stale'
    assert r3_stale.payload_tag == 30, f'expected PluginErrorPayload(30), got {r3_stale.payload_tag}'
    assert 'FENCED_OUT' in r3_stale.payload.message

    f3 = read_frame(out)
    assert f3 is not None
    r3 = Envelope.decode(f3)
    assert r3.request_id == 'req-3'
    assert r3.payload_tag == 21, f'expected InvokeResult(21), got {r3.payload_tag}'
    assert r3.payload.payload == b'ANALYSIS_OK:data123'
    assert worker.cancelled == ('req-3', 'deadline')

    f4 = read_frame(out)
    assert f4 is not None
    r4 = Envelope.decode(f4)
    assert r4.request_id == 'cancel-1'
    assert r4.payload_tag == 17
    assert isinstance(r4.payload, CancelAck)
    assert r4.payload.target_request_id == 'req-3'

    f5 = read_frame(out)
    assert f5 is not None
    r5 = Envelope.decode(f5)
    assert r5.request_id == 'req-4'
    assert r5.payload_tag == 15
    assert r5.payload.message == 'Shutdown ACK'

    print('Python Protobuf Envelope Codec, Lifecycle Loop & Fence Rotation VERIFIED!')

if __name__ == '__main__':
    main()
