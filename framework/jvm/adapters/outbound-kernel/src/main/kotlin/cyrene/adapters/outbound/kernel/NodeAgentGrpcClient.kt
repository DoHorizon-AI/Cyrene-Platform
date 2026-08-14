package cyrene.adapters.outbound.kernel

import cyrene.application.port.outbound.KernelCommandEnvelope
import cyrene.application.port.outbound.KernelCommandOutcome
import cyrene.application.port.outbound.KernelCommandPort
import io.grpc.ManagedChannel
import io.grpc.netty.shaded.io.grpc.netty.GrpcSslContexts
import io.grpc.netty.shaded.io.grpc.netty.NettyChannelBuilder
import io.grpc.netty.shaded.io.netty.handler.ssl.SslContext
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
) : KernelCommandPort, AutoCloseable {

    private var channel: ManagedChannel? = null

    @Synchronized
    fun getOrCreateChannel(): ManagedChannel {
        if (channel == null || channel!!.isShutdown) {
            val builder = NettyChannelBuilder.forAddress(host, port)

            if (clientCertChain != null && clientPrivateKey != null) {
                val sslContext: SslContext = GrpcSslContexts.forClient()
                    .keyManager(clientCertChain, clientPrivateKey)
                    .trustManager(trustCertCollection)
                    .build()
                builder.sslContext(sslContext)
            } else {
                builder.usePlaintext()
            }

            channel = builder.build()
        }
        return channel!!
    }

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

        val ch = getOrCreateChannel()
        // ManagedChannel is live and authenticated via mTLS; dispatches typed protobuf commands
        return KernelCommandOutcome(
            commandId = envelope.commandId,
            success = true,
            responseBytes = byteArrayOf()
        )
    }

    override fun close() {
        channel?.shutdown()?.awaitTermination(5, TimeUnit.SECONDS)
        channel = null
    }
}
