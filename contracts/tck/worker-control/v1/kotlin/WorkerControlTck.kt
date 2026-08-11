import java.io.File
import java.security.MessageDigest

/** Dependency-free Core v1 Worker-control conformance runner for Kotlin. */
object WorkerControlTck {
    private fun rows(path: File): List<List<String>> = path.readLines()
        .map { it.trim() }
        .filter { it.isNotEmpty() && !it.startsWith("#") }
        .map { it.split("|") }

    private fun decodeVarint(data: ByteArray, start: Int): Pair<Int, Int> {
        var value = 0
        var shift = 0
        var offset = start
        while (offset < data.size) {
            val byte = data[offset].toInt() and 0xff
            offset += 1
            value = value or ((byte and 0x7f) shl shift)
            if (byte < 0x80) return Pair(value, offset)
            shift += 7
        }
        error("truncated protobuf varint")
    }

    private fun hex(bytes: ByteArray): String =
        bytes.joinToString("") { "%02x".format(it.toInt() and 0xff) }

    private fun verifyVectors(root: File) {
        rows(File(root, "vectors.tsv")).forEach { (name, _, field, wireHex, expectedSha) ->
            check(wireHex == wireHex.lowercase()) { "$name: hex must be lower-case" }
            val wire = wireHex.chunked(2).map { it.toInt(16).toByte() }.toByteArray()
            check(hex(MessageDigest.getInstance("SHA-256").digest(wire)).uppercase() == expectedSha) {
                "$name: digest"
            }
            val (tag, afterTag) = decodeVarint(wire, 0)
            check((tag shr 3) == field.toInt() && (tag and 7) == 2) { "$name: oneof envelope" }
            val (payloadLength, afterLength) = decodeVarint(wire, afterTag)
            check(afterLength + payloadLength == wire.size) { "$name: bounded payload" }
        }
    }

    private fun traceResult(trace: String): String {
        var state = "AWAIT_HELLO"
        var generation = 0
        var nextHeartbeat = 1
        var shutdownId = ""
        trace.split(";").forEach { raw ->
            val (direction, kind, rawGeneration, rawSequence, identifier) = raw.split(":", limit = 5)
            val frameGeneration = rawGeneration.toInt()
            val sequence = rawSequence.toInt()
            when (state) {
                "AWAIT_HELLO" -> {
                    if (direction != "W2K" || kind != "HELLO" || frameGeneration == 0) return "HELLO_REQUIRED"
                    generation = frameGeneration
                    state = "AWAIT_WELCOME"
                }
                "AWAIT_WELCOME" -> {
                    if (direction != "K2W" || kind != "WELCOME" || frameGeneration != generation) return "WELCOME_REQUIRED"
                    state = "RUNNING"
                }
                "RUNNING" -> when {
                    direction == "W2K" && kind == "HEARTBEAT" -> {
                        if (frameGeneration != generation || sequence != nextHeartbeat) return "HEARTBEAT_SEQUENCE_INVALID"
                        nextHeartbeat += 1
                    }
                    direction == "K2W" && kind == "HEARTBEAT_ACK" -> {
                        if (frameGeneration != generation || sequence >= nextHeartbeat) return "HEARTBEAT_ACK_INVALID"
                    }
                    direction == "K2W" && kind == "SHUTDOWN" && identifier.isNotEmpty() -> {
                        shutdownId = identifier
                        state = "AWAIT_SHUTDOWN_ACK"
                    }
                    else -> return "FRAME_INVALID"
                }
                "AWAIT_SHUTDOWN_ACK" -> {
                    if (direction != "W2K" || kind != "SHUTDOWN_ACK" || frameGeneration != generation) return "SHUTDOWN_ACK_INVALID"
                    if (identifier != shutdownId) return "SHUTDOWN_ID_MISMATCH"
                    state = "STOPPED"
                }
                else -> return "FRAME_AFTER_STOP"
            }
        }
        return if (state == "STOPPED") "ACCEPT" else "TRACE_INCOMPLETE"
    }

    private fun verifyScenarios(root: File) {
        rows(File(root, "scenarios.tsv")).forEach { (name, expected, trace) ->
            check(traceResult(trace) == expected) { "$name: expected $expected" }
        }
    }

    private fun semanticTraceResult(trace: String): String {
        var state = "AWAIT_HELLO"
        var workerGeneration = 0
        var leaseGeneration = 0
        var fence = 0
        var shutdownId = ""
        trace.split(";").forEach { raw ->
            val (direction, kind, rawWorker, rawLease, rawFence, identifier) = raw.split(":", limit = 6)
            val current = Triple(rawWorker.toInt(), rawLease.toInt(), rawFence.toInt())
            val expected = Triple(workerGeneration, leaseGeneration, fence)
            when (state) {
                "AWAIT_HELLO" -> {
                    if (direction != "W2K" || kind != "HELLO" || 0 in listOf(current.first, current.second, current.third)) {
                        return "HELLO_REQUIRED"
                    }
                    workerGeneration = current.first
                    leaseGeneration = current.second
                    fence = current.third
                    state = "AWAIT_WELCOME"
                }
                "AWAIT_WELCOME" -> {
                    if (direction != "K2W" || kind != "WELCOME") return "WELCOME_REQUIRED"
                    if (current != expected) return "FENCE_MISMATCH"
                    state = "RUNNING"
                }
                "RUNNING" -> {
                    if (current != expected) return "FENCE_MISMATCH"
                    when {
                        direction == "W2K" && kind == "HEARTBEAT" -> Unit
                        direction == "K2W" && kind == "HEARTBEAT_ACK" -> Unit
                        direction == "K2W" && kind == "SHUTDOWN" && identifier.isNotEmpty() -> {
                            shutdownId = identifier
                            state = "AWAIT_SHUTDOWN_ACK"
                        }
                        else -> return "FRAME_INVALID"
                    }
                }
                "AWAIT_SHUTDOWN_ACK" -> {
                    if (current != expected) return "FENCE_MISMATCH"
                    if (direction != "W2K" || kind != "SHUTDOWN_ACK") return "SHUTDOWN_ACK_INVALID"
                    if (identifier != shutdownId) return "SHUTDOWN_ID_MISMATCH"
                    state = "STOPPED"
                }
                else -> return "FRAME_AFTER_STOP"
            }
        }
        return if (state == "STOPPED") "ACCEPT" else "TRACE_INCOMPLETE"
    }

    private fun verifySemanticScenarios(root: File) {
        rows(File(root, "semantic_scenarios.tsv")).forEach { (name, expected, trace) ->
            check(semanticTraceResult(trace) == expected) { "$name: expected $expected" }
        }
    }

    @JvmStatic
    fun main(args: Array<String>) {
        val root = File(if (args.isNotEmpty()) args[0] else ".")
        verifyVectors(root)
        verifyScenarios(root)
        verifySemanticScenarios(root)
        println("Kotlin Worker-control TCK v1 passed")
    }
}
