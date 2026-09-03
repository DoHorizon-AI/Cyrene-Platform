// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/jvm/adapters/outbound-kernel/src/main/kotlin/cyrene/adapters/outbound/kernel/NodeAgentGrpcClient.kt
// ║ Module: CYRENE Platform
// ║ Role: Kotlin Framework implementation or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kotlin Framework 实现或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
package cyrene.adapters.outbound.kernel

import cyrene.application.port.outbound.KernelCommandEnvelope
import cyrene.application.port.outbound.KernelCommandOutcome
import cyrene.application.port.outbound.KernelCommandPort
import io.grpc.CallOptions
import io.grpc.ManagedChannel
import io.grpc.MethodDescriptor
import io.grpc.StatusRuntimeException
import io.grpc.netty.shaded.io.grpc.netty.GrpcSslContexts
import io.grpc.netty.shaded.io.grpc.netty.NettyChannelBuilder
import io.grpc.netty.shaded.io.netty.handler.ssl.SslContext
import io.grpc.stub.ClientCalls
import java.io.ByteArrayInputStream
import java.io.File
import java.io.InputStream
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit

/**
 * Production gRPC Client connected to Rust `cy-node-agent` over mTLS.
 * Injects required ContractRevision, Request ID, Idempotency Key, and transport Principal,
 * and executes real gRPC binary/Protobuf RPC over HTTP/2 Netty Channel.
 */
class NodeAgentGrpcClient(
    val host: String = "127.0.0.1",
    val port: Int = 50052,
    val clientCertChain: File? = null,
    val clientPrivateKey: File? = null,
    val trustCertCollection: File? = null,
    val defaultTimeoutSeconds: Long = 30
) : KernelCommandPort, AutoCloseable {

    private var channel: ManagedChannel? = null

    private val methodDescriptors = ConcurrentHashMap<String, MethodDescriptor<ByteArray, ByteArray>>()

    /**
     * Obtains or registers a canonical MethodDescriptor for the targeted Service & RPC method
     * matching the cyrene.core.v1 contracts.
     */
    fun getMethodDescriptor(serviceName: String, methodName: String): MethodDescriptor<ByteArray, ByteArray> {
        val fullName = MethodDescriptor.generateFullMethodName(serviceName, methodName)
        return methodDescriptors.computeIfAbsent(fullName) {
            MethodDescriptor.newBuilder<ByteArray, ByteArray>()
                .setType(MethodDescriptor.MethodType.UNARY)
                .setFullMethodName(fullName)
                .setRequestMarshaller(ByteArrayMarshaller)
                .setResponseMarshaller(ByteArrayMarshaller)
                .build()
        }
    }

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
        val callOptions = CallOptions.DEFAULT.withDeadlineAfter(defaultTimeoutSeconds, TimeUnit.SECONDS)

        // Maps to canonical cyrene.core.v1.KernelAuthorityService actions
        val descriptor = getMethodDescriptor("cyrene.core.v1.KernelAuthorityService", envelope.commandId.ifBlank { "Execute" })

        return try {
            val responseBytes = ClientCalls.blockingUnaryCall(
                ch,
                descriptor,
                callOptions,
                envelope.payloadBytes
            )
            KernelCommandOutcome(
                commandId = envelope.commandId,
                success = true,
                responseBytes = responseBytes ?: byteArrayOf()
            )
        } catch (e: StatusRuntimeException) {
            KernelCommandOutcome(
                commandId = envelope.commandId,
                success = false,
                responseBytes = byteArrayOf(),
                errorCode = e.status.code.name,
                errorMessage = e.status.description ?: e.message
            )
        }
    }

    override fun close() {
        channel?.shutdown()?.awaitTermination(5, TimeUnit.SECONDS)
        channel = null
    }

    private object ByteArrayMarshaller : MethodDescriptor.Marshaller<ByteArray> {
        override fun stream(value: ByteArray): InputStream = ByteArrayInputStream(value)
        override fun parse(stream: InputStream): ByteArray = stream.readBytes()
    }
}
