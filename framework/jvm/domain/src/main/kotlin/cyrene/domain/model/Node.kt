package cyrene.domain.model

import java.time.Instant

/**
 * Pure Kotlin aggregate root representing a cluster worker Node.
 * Strictly free of Spring, Jakarta, DB, or gRPC annotations.
 */
data class Node(
    val id: String,
    val hostname: String,
    val generation: Long,
    val status: NodeStatus,
    val capabilities: Set<String> = emptySet(),
    val registeredAt: Instant,
    val lastHeartbeatAt: Instant
) {
    fun withHeartbeat(now: Instant, observedGeneration: Long): Node {
        require(observedGeneration >= generation) {
            "Observed node generation $observedGeneration cannot be smaller than current generation $generation"
        }
        return copy(
            generation = observedGeneration,
            lastHeartbeatAt = now,
            status = NodeStatus.READY
        )
    }

    fun markDegraded(): Node = copy(status = NodeStatus.DEGRADED)
}

enum class NodeStatus {
    DISCOVERED,
    READY,
    DEGRADED,
    DRAINING,
    OFFLINE
}
