// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/jvm/adapters/outbound-kernel/src/test/kotlin/cyrene/adapters/outbound/kernel/NodeAgentGrpcClientTest.kt
// ║ Module: CYRENE Platform
// ║ Role: Kotlin Framework implementation or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kotlin Framework 实现或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
package cyrene.adapters.outbound.kernel

import cyrene.application.port.outbound.KernelCommandEnvelope
import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotNull

class NodeAgentGrpcClientTest {

    @Test
    fun testRejectsMissingContractRevision() {
        val client = NodeAgentGrpcClient()
        val envelope = KernelCommandEnvelope(
            commandId = "AcquireLease",
            contractRevision = "", // missing!
            requestId = "req-1",
            idempotencyKey = "idem-1",
            principalUid = 1000
        )
        val ex = assertFailsWith<IllegalArgumentException> {
            client.executeCommand(envelope)
        }
        assertEquals("Kernel command rejected: missing selected ContractRevision", ex.message)
    }

    @Test
    fun testRejectsMissingRequestId() {
        val client = NodeAgentGrpcClient()
        val envelope = KernelCommandEnvelope(
            commandId = "AcquireLease",
            contractRevision = "core.v1.0",
            requestId = "", // missing!
            idempotencyKey = "idem-1",
            principalUid = 1000
        )
        val ex = assertFailsWith<IllegalArgumentException> {
            client.executeCommand(envelope)
        }
        assertEquals("Kernel command rejected: missing Request ID", ex.message)
    }

    @Test
    fun testRejectsMissingIdempotencyKey() {
        val client = NodeAgentGrpcClient()
        val envelope = KernelCommandEnvelope(
            commandId = "AcquireLease",
            contractRevision = "core.v1.0",
            requestId = "req-1",
            idempotencyKey = "", // missing!
            principalUid = 1000
        )
        val ex = assertFailsWith<IllegalArgumentException> {
            client.executeCommand(envelope)
        }
        assertEquals("Kernel command rejected: missing Idempotency Key", ex.message)
    }

    @Test
    fun testRejectsInvalidPrincipalUid() {
        val client = NodeAgentGrpcClient()
        val envelope = KernelCommandEnvelope(
            commandId = "AcquireLease",
            contractRevision = "core.v1.0",
            requestId = "req-1",
            idempotencyKey = "idem-1",
            principalUid = -1 // invalid!
        )
        val ex = assertFailsWith<IllegalArgumentException> {
            client.executeCommand(envelope)
        }
        assertEquals("Kernel command rejected: missing or invalid transport-injected Principal", ex.message)
    }

    @Test
    fun testCanonicalMethodDescriptorGeneration() {
        val client = NodeAgentGrpcClient()
        val desc = client.getMethodDescriptor("cyrene.core.v1.KernelAuthorityService", "StartWorker")
        assertNotNull(desc)
        assertEquals("cyrene.core.v1.KernelAuthorityService/StartWorker", desc.fullMethodName)
    }
}
