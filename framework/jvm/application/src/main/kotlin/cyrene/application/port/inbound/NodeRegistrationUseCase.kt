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
