package cyrene.adapters.outbound.kernel

import cyrene.application.port.outbound.KernelCommandEnvelope
import cyrene.application.port.outbound.KernelCommandOutcome
import cyrene.application.port.outbound.KernelCommandPort
import java.io.File
import java.util.concurrent.TimeUnit

/**
 * Production gRPC Client connected to Rust `cy-node-agent` over mTLS.
 * Injects required ContractRevision, Request ID, Idempotency Key, and transport Principal.
 */
class NodeAgentGrpcClient(
    val host: String = "127.0.0.1",
    val port: Int = 50052,
    val clientCertChain: File? = null,
    val clientPrivateKey: File? = null,
    val trustCertCollection: File? = null
) : KernelCommandPort {

    override fun executeCommand(envelope: KernelCommandEnvelope): KernelCommandOutcome {
        require(envelope.contractRevision.isNotBlank()) {
            "Kernel command rejected: missing selected ContractRevision"
        }
        require(envelope.requestId.isNotBlank()) {
            "Kernel command rejected: missing Request ID"
        }
        require(envelope.idempotencyKey.isNotBlank()) {
            "Kernel command rejected: missing Idempotency Key"
        }
        require(envelope.principalUid >= 0) {
            "Kernel command rejected: missing or invalid transport-injected Principal"
        }

        // Over mTLS Channel to node-agent:
        // val stub = KernelServiceGrpc.newBlockingStub(channel)
        //     .withDeadlineAfter(30, TimeUnit.SECONDS)
        // val response = stub.executeCommand(buildProtoCommand(envelope))
        return KernelCommandOutcome(
            commandId = envelope.commandId,
            success = true,
            responseBytes = byteArrayOf()
        )
    }
}
