import io
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))

from cyrene_worker import (
    CyreneWorker, Envelope, Hello, HealthCheck, Shutdown, Invoke,
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

def main():
    worker = MyWorker()
    inp = io.BytesIO()
    out = io.BytesIO()

    # 1. Hello
    e1 = Envelope(request_id='req-1', plugin_id='com.cy.analyzer', payload=Hello(min_protocol_version=1, max_protocol_version=1))
    write_frame(e1.encode(), inp)

    # 2. HealthCheck
    e2 = Envelope(request_id='req-2', plugin_id='com.cy.analyzer', payload=HealthCheck())
    write_frame(e2.encode(), inp)

    # 3. Invoke
    e3 = Envelope(request_id='req-3', plugin_id='com.cy.analyzer', payload=Invoke(capability='ModelAnalyzer', action='Inspect', payload=b'data123'))
    write_frame(e3.encode(), inp)

    # 4. Shutdown
    e4 = Envelope(request_id='req-4', plugin_id='com.cy.analyzer', payload=Shutdown(grace_period_ms=500))
    write_frame(e4.encode(), inp)

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

    f3 = read_frame(out)
    assert f3 is not None
    r3 = Envelope.decode(f3)
    assert r3.request_id == 'req-3'
    assert r3.payload_tag == 21, f'expected InvokeResult(21), got {r3.payload_tag}'
    assert r3.payload.payload == b'ANALYSIS_OK:data123'

    f4 = read_frame(out)
    assert f4 is not None
    r4 = Envelope.decode(f4)
    assert r4.request_id == 'req-4'
    assert r4.payload_tag == 15
    assert r4.payload.message == 'Shutdown ACK'

    print('Python Protobuf Envelope Codec & Lifecycle Loop VERIFIED!')

if __name__ == '__main__':
    main()
