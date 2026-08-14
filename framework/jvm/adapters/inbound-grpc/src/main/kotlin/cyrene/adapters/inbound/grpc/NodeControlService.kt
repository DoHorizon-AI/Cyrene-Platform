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
import java.io.ByteArrayOutputStream
import java.io.InputStream
import java.time.Instant
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

/**
 * Service implementing inbound NodeControl gRPC bidirectional stream and registration.
 */
class NodeControlService : NodeRegistrationUseCase, BindableService {

    private val activeNodes = ConcurrentHashMap<String, Node>()

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
                    createStreamHandler(responseObserver)
                }
            )
            .build()
    }

    fun createStreamHandler(responseObserver: StreamObserver<ByteArray>): StreamObserver<ByteArray> {
        return object : StreamObserver<ByteArray> {
            private var currentSessionId: String? = null
            private var outSequence = 0L

            override fun onNext(rawFrame: ByteArray) {
                try {
                    val frame = parseNodeToControlPlane(rawFrame)
                    when (frame.bodyTag) {
                        10 -> { // NodeHello
                            val hello = frame.hello ?: return
                            val (node, welcome) = registerNode(
                                NodeHelloCommand(
                                    nodeId = hello.nodeId,
                                    hostname = hello.nodeId,
                                    agentVersion = hello.agentVersion,
                                    protocolVersion = hello.maxProtocolVersion
                                ),
                                Instant.now()
                            )
                            currentSessionId = welcome.sessionId
                            activeNodes[node.id] = node

                            outSequence++
                            val welcomeFrame = serializeControlPlaneToNode(
                                frameId = UUID.randomUUID().toString(),
                                sequenceNumber = outSequence,
                                sessionId = welcome.sessionId,
                                welcome = welcome
                            )
                            responseObserver.onNext(welcomeFrame)
                        }
                        11 -> { // NodeHeartbeat
                            val hb = frame.heartbeat ?: return
                            activeNodes[hb.nodeId]?.let { existing ->
                                activeNodes[hb.nodeId] = existing.copy(lastHeartbeatAt = Instant.now())
                            }
                        }
                    }
                } catch (e: Exception) {
                    responseObserver.onError(e)
                }
            }

            override fun onError(t: Throwable) {
                responseObserver.onError(t)
            }

            override fun onCompleted() {
                responseObserver.onCompleted()
            }
        }
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

    fun getActiveNode(nodeId: String): Node? = activeNodes[nodeId]

    // --- Lightweight Zero-Dependency Protobuf Wire Helpers for NodeControl Protocol ---

    data class ParsedNodeHello(val nodeId: String, val agentVersion: String, val minProtocolVersion: Int, val maxProtocolVersion: Int)
    data class ParsedNodeHeartbeat(val nodeId: String, val observedGeneration: Long)
    data class ParsedNodeFrame(val frameId: String, val sequence: Long, val sessionId: String, val bodyTag: Int, val hello: ParsedNodeHello?, val heartbeat: ParsedNodeHeartbeat?)

    companion object {
        fun parseNodeToControlPlane(bytes: ByteArray): ParsedNodeFrame {
            var offset = 0
            var frameId = ""
            var seq = 0L
            var sessId = ""
            var bodyTag = 0
            var hello: ParsedNodeHello? = null
            var heartbeat: ParsedNodeHeartbeat? = null

            while (offset < bytes.size) {
                val (tag, nextOff) = decodeVarint(bytes, offset)
                offset = nextOff
                val fieldNum = (tag ushr 3).toInt()
                val wireType = (tag and 7).toInt()

                when {
                    fieldNum == 1 && wireType == 2 -> {
                        val (len, strOff) = decodeVarint(bytes, offset)
                        frameId = String(bytes, strOff, len.toInt(), Charsets.UTF_8)
                        offset = strOff + len.toInt()
                    }
                    fieldNum == 2 && wireType == 0 -> {
                        val (v, vOff) = decodeVarint(bytes, offset)
                        seq = v
                        offset = vOff
                    }
                    fieldNum == 3 && wireType == 2 -> {
                        val (len, strOff) = decodeVarint(bytes, offset)
                        sessId = String(bytes, strOff, len.toInt(), Charsets.UTF_8)
                        offset = strOff + len.toInt()
                    }
                    fieldNum == 10 && wireType == 2 -> { // NodeHello
                        bodyTag = 10
                        val (len, bodyOff) = decodeVarint(bytes, offset)
                        hello = parseNodeHello(bytes, bodyOff, len.toInt())
                        offset = bodyOff + len.toInt()
                    }
                    fieldNum == 11 && wireType == 2 -> { // NodeHeartbeat
                        bodyTag = 11
                        val (len, bodyOff) = decodeVarint(bytes, offset)
                        heartbeat = parseNodeHeartbeat(bytes, bodyOff, len.toInt())
                        offset = bodyOff + len.toInt()
                    }
                    wireType == 0 -> {
                        val (_, next) = decodeVarint(bytes, offset)
                        offset = next
                    }
                    wireType == 2 -> {
                        val (len, next) = decodeVarint(bytes, offset)
                        offset = next + len.toInt()
                    }
                    else -> offset++
                }
            }
            return ParsedNodeFrame(frameId, seq, sessId, bodyTag, hello, heartbeat)
        }

        private fun parseNodeHello(bytes: ByteArray, start: Int, length: Int): ParsedNodeHello {
            var offset = start
            val end = start + length
            var nodeId = ""
            var agentVer = ""
            var minProto = 1
            var maxProto = 1

            while (offset < end) {
                val (tag, nextOff) = decodeVarint(bytes, offset)
                offset = nextOff
                val fieldNum = (tag ushr 3).toInt()
                val wireType = (tag and 7).toInt()

                when {
                    fieldNum == 1 && wireType == 2 -> { // NodeRef
                        val (len, strOff) = decodeVarint(bytes, offset)
                        nodeId = parseNodeRefId(bytes, strOff, len.toInt())
                        offset = strOff + len.toInt()
                    }
                    fieldNum == 2 && wireType == 2 -> {
                        val (len, strOff) = decodeVarint(bytes, offset)
                        agentVer = String(bytes, strOff, len.toInt(), Charsets.UTF_8)
                        offset = strOff + len.toInt()
                    }
                    fieldNum == 3 && wireType == 0 -> {
                        val (v, next) = decodeVarint(bytes, offset)
                        minProto = v.toInt()
                        offset = next
                    }
                    fieldNum == 4 && wireType == 0 -> {
                        val (v, next) = decodeVarint(bytes, offset)
                        maxProto = v.toInt()
                        offset = next
                    }
                    wireType == 0 -> {
                        val (_, next) = decodeVarint(bytes, offset)
                        offset = next
                    }
                    wireType == 2 -> {
                        val (len, next) = decodeVarint(bytes, offset)
                        offset = next + len.toInt()
                    }
                    else -> offset++
                }
            }
            return ParsedNodeHello(nodeId, agentVer, minProto, maxProto)
        }

        private fun parseNodeHeartbeat(bytes: ByteArray, start: Int, length: Int): ParsedNodeHeartbeat {
            var offset = start
            val end = start + length
            var nodeId = ""
            var obsGen = 0L

            while (offset < end) {
                val (tag, nextOff) = decodeVarint(bytes, offset)
                offset = nextOff
                val fieldNum = (tag ushr 3).toInt()
                val wireType = (tag and 7).toInt()

                when {
                    fieldNum == 1 && wireType == 2 -> {
                        val (len, strOff) = decodeVarint(bytes, offset)
                        nodeId = parseNodeRefId(bytes, strOff, len.toInt())
                        offset = strOff + len.toInt()
                    }
                    fieldNum == 2 && wireType == 0 -> {
                        val (v, next) = decodeVarint(bytes, offset)
                        obsGen = v
                        offset = next
                    }
                    wireType == 0 -> {
                        val (_, next) = decodeVarint(bytes, offset)
                        offset = next
                    }
                    wireType == 2 -> {
                        val (len, next) = decodeVarint(bytes, offset)
                        offset = next + len.toInt()
                    }
                    else -> offset++
                }
            }
            return ParsedNodeHeartbeat(nodeId, obsGen)
        }

        private fun parseNodeRefId(bytes: ByteArray, start: Int, length: Int): String {
            var offset = start
            val end = start + length
            var id = ""
            while (offset < end) {
                val (tag, nextOff) = decodeVarint(bytes, offset)
                offset = nextOff
                val fieldNum = (tag ushr 3).toInt()
                val wireType = (tag and 7).toInt()
                if (fieldNum == 1 && wireType == 2) {
                    val (len, strOff) = decodeVarint(bytes, offset)
                    id = String(bytes, strOff, len.toInt(), Charsets.UTF_8)
                    offset = strOff + len.toInt()
                } else if (wireType == 0) {
                    val (_, next) = decodeVarint(bytes, offset)
                    offset = next
                } else if (wireType == 2) {
                    val (len, next) = decodeVarint(bytes, offset)
                    offset = next + len.toInt()
                } else offset++
            }
            return id
        }

        fun serializeControlPlaneToNode(
            frameId: String,
            sequenceNumber: Long,
            sessionId: String,
            welcome: NodeWelcomeResult
        ): ByteArray {
            val bos = ByteArrayOutputStream()
            // 1: frame_id
            writeString(bos, 1, frameId)
            // 2: sequence_number
            writeVarint(bos, (2 shl 3) or 0)
            writeVarint(bos, sequenceNumber)
            // 3: session_id
            writeString(bos, 3, sessionId)

            // 10: NodeWelcome
            val welcomeBos = ByteArrayOutputStream()
            writeString(welcomeBos, 1, welcome.sessionId)
            writeVarint(welcomeBos, (2 shl 3) or 0)
            writeVarint(welcomeBos, welcome.selectedProtocolVersion.toLong())
            writeVarint(welcomeBos, (3 shl 3) or 0)
            writeVarint(welcomeBos, welcome.desiredGeneration)

            val welcomeBytes = welcomeBos.toByteArray()
            writeVarint(bos, (10 shl 3) or 2)
            writeVarint(bos, welcomeBytes.size.toLong())
            bos.write(welcomeBytes)

            return bos.toByteArray()
        }

        fun decodeVarint(bytes: ByteArray, offset: Int): Pair<Long, Int> {
            var result = 0L
            var shift = 0
            var cur = offset
            while (cur < bytes.size) {
                val b = bytes[cur++].toInt()
                result = result or ((b and 0x7F).toLong() shl shift)
                if ((b and 0x80) == 0) {
                    return Pair(result, cur)
                }
                shift += 7
            }
            return Pair(result, cur)
        }

        fun writeVarint(bos: ByteArrayOutputStream, value: Long) {
            var v = value
            while (v >= 0x80) {
                bos.write(((v and 0x7F) or 0x80).toInt())
                v = v ushr 7
            }
            bos.write((v and 0x7F).toInt())
        }

        fun writeString(bos: ByteArrayOutputStream, fieldNum: Int, str: String) {
            if (str.isEmpty()) return
            val bytes = str.toByteArray(Charsets.UTF_8)
            writeVarint(bos, (fieldNum.toLong() shl 3) or 2)
            writeVarint(bos, bytes.size.toLong())
            bos.write(bytes)
        }
    }

    private object ByteArrayMarshaller : MethodDescriptor.Marshaller<ByteArray> {
        override fun stream(value: ByteArray): InputStream = ByteArrayInputStream(value)
        override fun parse(stream: InputStream): ByteArray = stream.readBytes()
    }
}
