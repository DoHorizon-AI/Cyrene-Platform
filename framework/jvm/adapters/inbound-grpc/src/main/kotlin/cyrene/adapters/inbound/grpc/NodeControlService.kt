package cyrene.adapters.inbound.grpc

import cyrene.application.port.inbound.NodeHelloCommand
import cyrene.application.port.inbound.NodeRegistrationUseCase
import cyrene.application.port.inbound.NodeWelcomeResult
import cyrene.domain.model.Node
import cyrene.domain.model.NodeStatus
import io.grpc.BindableService
import io.grpc.MethodDescriptor
import io.grpc.ServerServiceDefinition
import io.grpc.stub.ServerCalls
import io.grpc.stub.StreamObserver
import java.io.ByteArrayInputStream
import java.io.InputStream
import java.time.Instant
import java.util.UUID

/**
 * Service implementing inbound NodeControl gRPC bidirectional stream and registration.
 */
class NodeControlService : NodeRegistrationUseCase, BindableService {

    val connectMethod: MethodDescriptor<ByteArray, ByteArray> =
        MethodDescriptor.newBuilder<ByteArray, ByteArray>()
            .setType(MethodDescriptor.MethodType.BIDI_STREAMING)
            .setFullMethodName(MethodDescriptor.generateFullMethodName("cyrene.core.v1.NodeControlService", "Connect"))
            .setRequestMarshaller(ByteArrayMarshaller)
            .setResponseMarshaller(ByteArrayMarshaller)
            .build()

    override fun bindService(): ServerServiceDefinition {
        return ServerServiceDefinition.builder("cyrene.core.v1.NodeControlService")
            .addMethod(
                connectMethod,
                ServerCalls.asyncBidiStreamingCall { responseObserver: StreamObserver<ByteArray> ->
                    object : StreamObserver<ByteArray> {
                        override fun onNext(rawFrame: ByteArray) {
                            // In production bidi stream, handle NodeToControlPlane frames (Hello, Heartbeat, CommandResult)
                            // and respond with ControlPlaneToNode frames
                        }

                        override fun onError(t: Throwable) {
                            responseObserver.onError(t)
                        }

                        override fun onCompleted() {
                            responseObserver.onCompleted()
                        }
                    }
                }
            )
            .build()
    }

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

    private object ByteArrayMarshaller : MethodDescriptor.Marshaller<ByteArray> {
        override fun stream(value: ByteArray): InputStream = ByteArrayInputStream(value)
        override fun parse(stream: InputStream): ByteArray = stream.readBytes()
    }
}
