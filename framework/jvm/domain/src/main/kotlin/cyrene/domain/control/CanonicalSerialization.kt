package cyrene.domain.control

/** Small RFC-8785-compatible serializer for the ASCII control-plane fields. */
internal fun quoteCanonical(value: String): String = buildString {
    append('"')
    value.forEach { character ->
        when (character) {
            '"' -> append("\\\"")
            '\\' -> append("\\\\")
            '\b' -> append("\\b")
            '\t' -> append("\\t")
            '\n' -> append("\\n")
            '\u000C' -> append("\\f")
            '\r' -> append("\\r")
            in '\u0000'..'\u001F' -> append("\\u%04x".format(character.code))
            else -> append(character)
        }
    }
    append('"')
}

internal fun canonicalObject(fields: Map<String, String>): String = buildString {
    append('{')
    fields.toSortedMap().entries.forEachIndexed { index, (key, value) ->
        if (index > 0) append(',')
        append(quoteCanonical(key)).append(':').append(value)
    }
    append('}')
}

internal fun canonicalArray(values: List<String>): String = values.joinToString(",", "[", "]")

internal fun canonicalNullable(value: String?): String = value?.let(::quoteCanonical) ?: "null"

internal fun canonicalMap(values: Map<String, String>): String = canonicalObject(
    values.mapValues { (_, value) -> quoteCanonical(value) }
)

fun PlanStep.canonicalIdentityJson(): String = canonicalObject(
    mapOf(
        "step_id" to quoteCanonical(stepId),
        "capability" to quoteCanonical(capability),
        "inputs" to canonicalArray(
            inputs.sortedBy { "${it.uri}|${it.digest}|${it.kind}" }.map {
                canonicalObject(
                    mapOf(
                        "uri" to quoteCanonical(it.uri),
                        "digest" to quoteCanonical(it.digest),
                        "kind" to quoteCanonical(it.kind),
                    )
                )
            }
        ),
        "outputs" to canonicalArray(outputs.sorted().map(::quoteCanonical)),
        "environment_identity" to canonicalNullable(environmentIdentity),
        "resource_reference" to canonicalNullable(resourceReference),
        "dependencies" to canonicalArray(
            dependencies.sortedBy { it.stepId }.map {
                canonicalObject(
                    mapOf(
                        "step_id" to quoteCanonical(it.stepId),
                        "required_status" to quoteCanonical(it.requiredStatus.name.lowercase()),
                    )
                )
            }
        ),
        "retry_policy" to canonicalObject(
            mapOf(
                "max_attempts" to retryPolicy.maxAttempts.toString(),
                "retry_failed" to retryPolicy.retryFailed.toString(),
                "retry_lost" to retryPolicy.retryLost.toString(),
            )
        ),
        "execution_payload_ref" to canonicalNullable(executionPayloadReference),
        "metadata" to canonicalMap(metadata),
        "provenance" to canonicalMap(provenance),
    )
)

fun Attempt.canonicalJson(): String = canonicalObject(
    mapOf(
        "attempt_id" to quoteCanonical(attemptId),
        "run_id" to quoteCanonical(runId),
        "number" to number.toString(),
        "step_id" to quoteCanonical(stepId),
        "status" to quoteCanonical(status.name.lowercase()),
        "execution_references" to canonicalArray(executionReferences.map(::quoteCanonical)),
        "cleanup_confirmed" to cleanupConfirmed.toString(),
        "error" to canonicalNullable(error),
        "metadata" to canonicalMap(metadata),
        "created_at" to quoteCanonical(createdAt.toString()),
        "observed_at" to quoteCanonical(observedAt.toString()),
    )
)
