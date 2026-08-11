import java.io.File

/** Dependency-free CYRENE Kernel Semantic Contract v1 runner. */
object KernelSemanticTck {
    private val limits = mapOf(
        "max_id_bytes" to 256L,
        "max_namespaced_id_bytes" to 128L,
        "max_capabilities" to 64L,
        "max_properties" to 64L,
        "max_resources_per_snapshot" to 1024L,
        "max_resources_per_lease" to 256L,
        "max_workers_per_snapshot" to 4096L,
        "max_endpoints_per_snapshot" to 4096L,
        "max_execution_ref_bytes" to 512L,
        "max_error_message_bytes" to 1024L,
        "max_event_body_bytes" to 65536L,
        "max_events_per_page" to 256L,
        "max_timestamp_unix_ms" to 253402300799999L,
    )
    private val states = mapOf(
        "lease" to listOf("ACTIVE", "RELEASING", "RELEASED", "EXPIRED", "REVOKED", "FAILED"),
        "worker" to listOf("REGISTERED", "STARTING", "RUNNING", "DRAINING", "STOPPED", "FAILED", "LOST"),
        "operation" to listOf("CREATED", "PENDING", "RUNNING", "SUCCEEDED", "FAILED", "CANCELLING", "CANCELLED", "LOST"),
    )

    private fun rows(root: File, name: String): List<List<String>> = File(root, name).readLines()
        .map { it.trim() }
        .filter { it.isNotEmpty() && !it.startsWith("#") }
        .map { it.split("|") }

    private fun namespacedResult(value: String): String {
        val bytes = value.toByteArray(Charsets.UTF_8)
        if (bytes.isEmpty() || bytes.size > limits.getValue("max_namespaced_id_bytes")) return "TEXT_INVALID"
        var expectStart = true
        bytes.forEach { raw ->
            val byte = raw.toInt() and 0xff
            if (byte == '.'.code || byte == '-'.code || byte == '_'.code) {
                if (expectStart) return "NAMESPACED_ID_INVALID"
                expectStart = true
            } else if (expectStart) {
                if (byte !in 'a'.code..'z'.code) return "NAMESPACED_ID_INVALID"
                expectStart = false
            } else if (byte !in 'a'.code..'z'.code && byte !in '0'.code..'9'.code) {
                return "NAMESPACED_ID_INVALID"
            }
        }
        return if (expectStart) "NAMESPACED_ID_INVALID" else "ACCEPT"
    }

    private fun identityResult(raw: String): String {
        val value = when (raw) {
            "<empty>" -> ""
            "<c0>" -> "\u0001"
            "<c1>" -> "\u0085"
            else -> raw
        }
        val bytes = value.toByteArray(Charsets.UTF_8)
        return if (bytes.isNotEmpty() && bytes.size <= limits.getValue("max_id_bytes") && value.none { it.isISOControl() }) "ACCEPT" else "TEXT_INVALID"
    }

    private fun verifyIdentifiers(root: File) {
        rows(root, "identifiers.tsv").forEach { (name, kind, value, expected) ->
            val actual = when (kind) {
                "namespaced" -> namespacedResult(value)
                "identity" -> identityResult(value)
                "timestamp" -> if (value.toLong() in 1..limits.getValue("max_timestamp_unix_ms")) "ACCEPT" else "TIMESTAMP_INVALID"
                else -> error("$name: unknown identifier vector kind")
            }
            check(actual == expected) { "$name: expected $expected, got $actual" }
        }
    }

    private fun verifyLimits(root: File) {
        val fixture = rows(root, "limits.tsv").associate { (name, value) -> name to value.toLong() }
        check(fixture == limits) { "limit drift: $fixture" }
    }

    private fun verifyTransitions(root: File) {
        val seen = mutableSetOf<Pair<String, String>>()
        rows(root, "transitions.tsv").forEach { (noun, source, allowedRaw) ->
            val vocabulary = states[noun] ?: error("$noun: unknown noun")
            check(source in vocabulary) { "$noun.$source: unknown state" }
            val allowed = allowedRaw.split(",")
            check(allowed.size == allowed.toSet().size) { "$noun.$source: duplicate target" }
            check(allowed.all { it in vocabulary }) { "$noun.$source: unknown target" }
            check(source in allowed) { "$noun.$source: idempotent replay missing" }
            seen.add(noun to source)
        }
        val expected = states.flatMap { (noun, values) -> values.map { noun to it } }.toSet()
        check(seen == expected) { "transition matrix is incomplete" }
    }

    private fun verifyNegotiation(root: File) {
        rows(root, "negotiation.tsv").forEach { row ->
            val name = row[0]
            val localId = row[1]
            val localMajor = row[2]
            val localMinor = row[3]
            val offeredId = row[4]
            val offeredMajor = row[5]
            val offeredMinor = row[6]
            val expected = row[7]
            val compatible = namespacedResult(localId) == "ACCEPT" && namespacedResult(offeredId) == "ACCEPT" &&
                localMajor.toLong() > 0 && offeredMajor.toLong() > 0 && localId == offeredId && localMajor == offeredMajor
            val actual = if (compatible) "$localMajor.${minOf(localMinor.toLong(), offeredMinor.toLong())}" else "INCOMPATIBLE"
            check(actual == expected) { "$name: negotiation decision" }
        }
    }

    private fun properties(raw: String): Map<String, String> =
        if (raw == "-") emptyMap() else raw.split(",").associate { item -> item.split("=", limit = 2).let { it[0] to it[1] } }

    private fun capacity(raw: String): Map<String, Pair<Long, String>> {
        if (raw == "-") return emptyMap()
        return raw.split(",").associate { item ->
            val (key, quantity) = item.split("=", limit = 2)
            val (value, unit) = quantity.split("@", limit = 2)
            key to (value.toLong() to unit)
        }
    }

    private fun verifyMatching(root: File) {
        rows(root, "matching.tsv").forEach { row ->
            val name = row[0]
            val providedId = row[1]
            val revision = row[2]
            val providedRaw = row[3]
            val requiredId = row[4]
            val minimumRevision = row[5]
            val requiredRaw = row[6]
            val capacityRaw = row[7]
            val minimumRaw = row[8]
            val expected = row[9]
            val provided = properties(providedRaw)
            var actual = providedId == requiredId && revision.toLong() >= minimumRevision.toLong() &&
                properties(requiredRaw).all { (key, value) -> provided[key] == value }
            val providedCapacity = capacity(capacityRaw)
            capacity(minimumRaw).forEach { (key, required) ->
                val current = providedCapacity[key]
                actual = actual && current != null && current.second == required.second && current.first >= required.first
            }
            check(actual == (expected == "true")) { "$name: matching decision" }
        }
    }

    private fun verifyAuthority(root: File) {
        rows(root, "authority.tsv").forEach { row ->
            val name = row[0]
            val kind = row[1]
            val state = row[2]
            val identityMatch = row[3]
            val resourceMatch = row[4]
            val leaseIdentityMatch = row[5]
            val fenceMatch = row[6]
            val leaseExpiry = row[7]
            val grantExpiry = row[8]
            val now = row[9]
            val expected = row[10]
            val nowValue = now.toLong()
            var actual = state == "ACTIVE" && identityMatch == "true" && resourceMatch == "true" &&
                leaseIdentityMatch == "true" && fenceMatch == "true" && nowValue < leaseExpiry.toLong()
            actual = when (kind) {
                "lease" -> actual
                "grant" -> actual && nowValue < grantExpiry.toLong()
                else -> error("$name: unknown authority vector kind")
            }
            check(actual == (expected == "true")) { "$name: authority decision" }
        }
    }

    private fun verifyReplay(root: File) {
        rows(root, "replay.tsv").forEach { row ->
            val name = row[0]
            val sourceMatches = row[1]
            val cursor = row[2]
            val oldest = row[3]
            val latest = row[4]
            val limit = row[5]
            val expectedStatus = row[6]
            val expectedSequences = row[7]
            val cursorValue = cursor.toLong()
            val oldestValue = oldest.toLong()
            val latestValue = latest.toLong()
            val (status, sequences) = when {
                sourceMatches != "true" -> "SOURCE_CHANGED" to emptyList()
                cursorValue + 1 < oldestValue -> "GAP" to emptyList()
                else -> "CURRENT" to (maxOf(cursorValue + 1, oldestValue)..latestValue).take(limit.toInt())
            }
            val actualSequences = if (sequences.isEmpty()) "-" else sequences.joinToString(",")
            check(status == expectedStatus && actualSequences == expectedSequences) { "$name: replay decision" }
        }
    }

    private fun verifyRenewal(root: File) {
        rows(root, "renewal.tsv").forEach { row ->
            val name = row[0]
            val state = row[1]
            val fenceMatches = row[2]
            val currentExpiry = row[3].toLong()
            val newExpiry = row[4].toLong()
            val now = row[5].toLong()
            val expected = row[6]
            val actual = when {
                state != "ACTIVE" -> "LEASE_NOT_ACTIVE"
                now >= currentExpiry -> "LEASE_EXPIRED"
                fenceMatches != "true" -> "FENCE_MISMATCH"
                newExpiry <= currentExpiry -> "LEASE_RENEWAL_INVALID"
                else -> "ACCEPT"
            }
            check(actual == expected) { "$name: renewal decision" }
        }
    }

    @JvmStatic
    fun main(args: Array<String>) {
        val root = File(if (args.isNotEmpty()) args[0] else ".")
        verifyLimits(root)
        verifyIdentifiers(root)
        verifyNegotiation(root)
        verifyTransitions(root)
        verifyMatching(root)
        verifyAuthority(root)
        verifyReplay(root)
        verifyRenewal(root)
        println("Kotlin Kernel Semantic TCK v1 passed")
    }
}
