package cyrene.application.port.outbound

data class KernelCommandEnvelope(
    val commandId: String,
    val requestId: String,
    val idempotencyKey: String,
    val contractRevision: String,
    val principalUid: Long,
    val principalGid: Long,
    val actionName: String,
    val payloadBytes: ByteArray
)

data class KernelCommandOutcome(
    val commandId: String,
    val success: Boolean,
    val responseBytes: ByteArray,
    val errorCode: String? = null,
    val errorMessage: String? = null
)

interface KernelCommandPort {
    fun executeCommand(envelope: KernelCommandEnvelope): KernelCommandOutcome
}
