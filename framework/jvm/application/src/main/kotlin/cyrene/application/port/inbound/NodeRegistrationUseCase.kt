// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/jvm/application/src/main/kotlin/cyrene/application/port/inbound/NodeRegistrationUseCase.kt
// ║ Module: CYRENE Platform
// ║ Role: Kotlin Framework implementation or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kotlin Framework 实现或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
package cyrene.application.port.inbound

import cyrene.domain.model.Node
import java.time.Instant

data class NodeHelloCommand(
    val nodeId: String,
    val hostname: String,
    val agentVersion: String,
    val protocolVersion: Int
)

data class NodeWelcomeResult(
    val sessionId: String,
    val selectedProtocolVersion: Int,
    val desiredGeneration: Long,
    val heartbeatIntervalMs: Long
)

interface NodeRegistrationUseCase {
    fun registerNode(command: NodeHelloCommand, now: Instant): Pair<Node, NodeWelcomeResult>
}
