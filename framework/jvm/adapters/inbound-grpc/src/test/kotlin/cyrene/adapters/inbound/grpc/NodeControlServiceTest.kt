// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/jvm/adapters/inbound-grpc/src/test/kotlin/cyrene/adapters/inbound/grpc/NodeControlServiceTest.kt
// ║ Module: CYRENE Platform
// ║ Role: Kotlin Framework implementation or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kotlin Framework 实现或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
package cyrene.adapters.inbound.grpc

import io.grpc.stub.StreamObserver
import java.io.ByteArrayOutputStream
import java.util.concurrent.atomic.AtomicReference
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

class NodeControlServiceTest {

    @Test
    fun testMockNodeAgentConnectHandshakeAndHeartbeat() {
        val service = NodeControlService()
        val receivedResponse = AtomicReference<ByteArray>()
        val completed = AtomicReference<Boolean>(false)

        val responseObserver = object : StreamObserver<ByteArray> {
            override fun onNext(value: ByteArray) {
                receivedResponse.set(value)
            }

            override fun onError(t: Throwable) {
                throw t
            }

            override fun onCompleted() {
                completed.set(true)
            }
        }

        val requestObserver = service.createStreamHandler(responseObserver)

        // 1. Construct Mock NodeHello Frame from node-agent
        val helloFrameBytes = buildMockNodeHelloFrame(
            nodeId = "test-node-alpha",
            agentVersion = "1.0.0",
            maxProtocolVersion = 1
        )

        // 2. Send NodeHello to Control Plane
        requestObserver.onNext(helloFrameBytes)

        val responseBytes = receivedResponse.get()
        assertNotNull(responseBytes, "Control plane must respond to NodeHello with NodeWelcome")

        // 3. Parse and assert NodeWelcome response
        val (sessionId, welcome) = parseMockWelcomeFrame(responseBytes)
        assertTrue(sessionId.isNotBlank(), "Session ID must be established")
        assertNotNull(welcome, "NodeWelcome body must be present")
        assertEquals(1, welcome.selectedProtocolVersion)

        // Verify active node is registered in domain state
        val registeredNode = service.getActiveNode("test-node-alpha")
        assertNotNull(registeredNode)
        assertEquals("test-node-alpha", registeredNode.id)

        // 4. Send Mock NodeHeartbeat Frame
        val heartbeatFrameBytes = buildMockNodeHeartbeatFrame(
            nodeId = "test-node-alpha",
            sessionId = sessionId,
            observedGeneration = 1L
        )
        requestObserver.onNext(heartbeatFrameBytes)

        // 5. Complete Stream
        requestObserver.onCompleted()
        assertTrue(completed.get())
    }

    private fun buildMockNodeHelloFrame(nodeId: String, agentVersion: String, maxProtocolVersion: Int): ByteArray {
        val bos = ByteArrayOutputStream()
        // 1: frame_id
        NodeControlService.writeString(bos, 1, "frame-hello-1")
        // 2: sequence_number
        NodeControlService.writeVarint(bos, (2 shl 3) or 0)
        NodeControlService.writeVarint(bos, 1L)

        // 10: NodeHello
        val helloBos = ByteArrayOutputStream()
        // 1: node (NodeRef)
        val nodeRefBos = ByteArrayOutputStream()
        NodeControlService.writeString(nodeRefBos, 1, nodeId)
        val nodeRefBytes = nodeRefBos.toByteArray()
        NodeControlService.writeVarint(helloBos, (1 shl 3) or 2)
        NodeControlService.writeVarint(helloBos, nodeRefBytes.size.toLong())
        helloBos.write(nodeRefBytes)

        // 2: agent_version
        NodeControlService.writeString(helloBos, 2, agentVersion)
        // 3: min_protocol_version
        NodeControlService.writeVarint(helloBos, (3 shl 3) or 0)
        NodeControlService.writeVarint(helloBos, 1L)
        // 4: max_protocol_version
        NodeControlService.writeVarint(helloBos, (4 shl 3) or 0)
        NodeControlService.writeVarint(helloBos, maxProtocolVersion.toLong())

        val helloBytes = helloBos.toByteArray()
        NodeControlService.writeVarint(bos, (10 shl 3) or 2)
        NodeControlService.writeVarint(bos, helloBytes.size.toLong())
        bos.write(helloBytes)

        return bos.toByteArray()
    }

    private fun buildMockNodeHeartbeatFrame(nodeId: String, sessionId: String, observedGeneration: Long): ByteArray {
        val bos = ByteArrayOutputStream()
        NodeControlService.writeString(bos, 1, "frame-hb-1")
        NodeControlService.writeVarint(bos, (2 shl 3) or 0)
        NodeControlService.writeVarint(bos, 2L)
        NodeControlService.writeString(bos, 3, sessionId)

        val hbBos = ByteArrayOutputStream()
        val nodeRefBos = ByteArrayOutputStream()
        NodeControlService.writeString(nodeRefBos, 1, nodeId)
        val nodeRefBytes = nodeRefBos.toByteArray()
        NodeControlService.writeVarint(hbBos, (1 shl 3) or 2)
        NodeControlService.writeVarint(hbBos, nodeRefBytes.size.toLong())
        hbBos.write(nodeRefBytes)

        NodeControlService.writeVarint(hbBos, (2 shl 3) or 0)
        NodeControlService.writeVarint(hbBos, observedGeneration)

        val hbBytes = hbBos.toByteArray()
        NodeControlService.writeVarint(bos, (11 shl 3) or 2)
        NodeControlService.writeVarint(bos, hbBytes.size.toLong())
        bos.write(hbBytes)

        return bos.toByteArray()
    }

    private data class MockWelcome(val selectedProtocolVersion: Int, val desiredGeneration: Long)

    private fun parseMockWelcomeFrame(bytes: ByteArray): Pair<String, MockWelcome?> {
        var offset = 0
        var sessionId = ""
        var welcome: MockWelcome? = null

        while (offset < bytes.size) {
            val (tag, nextOff) = NodeControlService.decodeVarint(bytes, offset)
            offset = nextOff
            val fieldNum = (tag ushr 3).toInt()
            val wireType = (tag and 7).toInt()

            when {
                fieldNum == 3 && wireType == 2 -> {
                    val (len, strOff) = NodeControlService.decodeVarint(bytes, offset)
                    sessionId = String(bytes, strOff, len.toInt(), Charsets.UTF_8)
                    offset = strOff + len.toInt()
                }
                fieldNum == 10 && wireType == 2 -> { // NodeWelcome
                    val (len, bodyOff) = NodeControlService.decodeVarint(bytes, offset)
                    var subOff = bodyOff
                    val subEnd = bodyOff + len.toInt()
                    var selProto = 1
                    var desGen = 1L
                    while (subOff < subEnd) {
                        val (sTag, sNext) = NodeControlService.decodeVarint(bytes, subOff)
                        subOff = sNext
                        when ((sTag ushr 3).toInt()) {
                            2 -> {
                                val (v, vOff) = NodeControlService.decodeVarint(bytes, subOff)
                                selProto = v.toInt()
                                subOff = vOff
                            }
                            3 -> {
                                val (v, vOff) = NodeControlService.decodeVarint(bytes, subOff)
                                desGen = v
                                subOff = vOff
                            }
                            else -> subOff++
                        }
                    }
                    welcome = MockWelcome(selProto, desGen)
                    offset = subEnd
                }
                wireType == 0 -> {
                    val (_, next) = NodeControlService.decodeVarint(bytes, offset)
                    offset = next
                }
                wireType == 2 -> {
                    val (len, next) = NodeControlService.decodeVarint(bytes, offset)
                    offset = next + len.toInt()
                }
                else -> offset++
            }
        }
        return Pair(sessionId, welcome)
    }
}
