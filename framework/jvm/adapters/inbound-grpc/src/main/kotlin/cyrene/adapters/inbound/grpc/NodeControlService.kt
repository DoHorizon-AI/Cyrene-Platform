package cyrene.adapters.inbound.grpc

import cyrene.application.port.inbound.NodeHelloCommand
import cyrene.application.port.inbound.NodeRegistrationUseCase
import cyrene.application.port.inbound.NodeWelcomeResult
import cyrene.domain.model.Node
import cyrene.domain.model.NodeStatus
import java.time.Instant
import java.util.UUID

/**
 * Service implementing inbound NodeControl handshake and registration.
 */
class NodeControlService : NodeRegistrationUseCase {

    override fun registerNode(command: NodeHelloCommand, now: Instant): Pair<Node, NodeWelcomeResult> {
        val node = Node(
            id = command.nodeId,
            hostname = command.hostname,
            generation = 1L,
            status = NodeStatus.READY,
            capabilities = setOf("compute.cpu", "memory.ram"),
            registeredAt = now,
            lastHeartbeatAt = now
        )

        val welcome = NodeWelcomeResult(
            sessionId = UUID.randomUUID().toString(),
            selectedProtocolVersion = command.protocolVersion.coerceAtMost(1),
            desiredGeneration = 1L,
            heartbeatIntervalMs = 5000L
        )

        return Pair(node, welcome)
    }
}
