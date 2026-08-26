package cyrene.domain.control

import java.nio.file.Files
import java.nio.file.Path
import java.time.Instant
import kotlin.test.Test
import kotlin.test.assertEquals

class ControlPlaneConformanceTest {
    @Test
    fun sharedFixtureMatchesPlanAndAttemptCanonicalSerialization() {
        val fixture = Files.readString(fixturePath())
        val expectedPlan = jsonString(fixture, "plan_identity_canonical")
        val expectedAttempt = jsonString(fixture, "attempt_canonical")

        val step = PlanStep(
            stepId = "step-prepare",
            capability = "training.engine.v1",
            outputs = listOf("result"),
        )
        val plan = ExecutionPlan.deterministic(listOf(step))
        assertEquals(expectedPlan, plan.canonicalIdentityJson(), "actual=${plan.canonicalIdentityJson()}")

        val attempt = Attempt(
            attemptId = "run-conformance:1",
            runId = "run-conformance",
            number = 1,
            stepId = "step-prepare",
            createdAt = Instant.parse("2026-01-01T00:00:00Z"),
            observedAt = Instant.parse("2026-01-01T00:00:00Z"),
        )
        assertEquals(expectedAttempt, attempt.canonicalJson())
    }

    @Test
    fun sharedFixturePreservesAttemptIdentityGenerationAndIdempotencyKey() {
        val fixture = Files.readString(fixturePath())
        assertEquals("run-conformance:1", jsonString(fixture, "attempt_id"))
        assertEquals("run-conformance", jsonString(fixture, "run_id"))
        assertEquals("conformance:fixed", jsonString(fixture, "idempotency_key"))
        assertEquals(7L, numberValue(fixture, "generation"))
    }

    private fun fixturePath(): Path {
        var directory: Path? = Path.of("").toAbsolutePath()
        while (directory != null) {
            val candidate = directory.resolve(
                "contracts/fixtures/control-plane-v1/conformance.json"
            )
            if (Files.exists(candidate)) return candidate
            directory = directory.parent
        }
        error("control-plane conformance fixture was not found")
    }

    private fun jsonString(json: String, key: String): String {
        val match = Regex("\\\"$key\\\"\\s*:\\s*\\\"((?:\\\\.|[^\\\"\\\\])*)\\\"").find(json)
            ?: error("missing fixture key $key")
        return match.groupValues[1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    }

    private fun numberValue(json: String, key: String): Long {
        val match = Regex("\\\"$key\\\"\\s*:\\s*(\\d+)").find(json)
            ?: error("missing fixture key $key")
        return match.groupValues[1].toLong()
    }
}
