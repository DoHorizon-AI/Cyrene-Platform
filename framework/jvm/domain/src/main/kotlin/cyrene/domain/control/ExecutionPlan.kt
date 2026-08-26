package cyrene.domain.control

import java.security.MessageDigest

/**
 * Stable, Product-neutral Control Plane contract above Kernel Operations.
 * Products attach opaque payload references; this domain never interprets Product fields.
 */
const val CONTROL_PLANE_CONTRACT_VERSION = "cyrene.control-plane.v1"

enum class StepStatus { PENDING, RUNNING, SUCCEEDED, FAILED, BLOCKED, CANCELLING, CANCELLED }

enum class PlanStatus {
    PENDING, RUNNING, AWAITING_RETRY, SUCCEEDED, FAILED, BLOCKED, CANCEL_REQUESTED, CANCELLING, CANCELLED
}

data class ArtifactReference(val uri: String, val digest: String, val kind: String = "")

data class StepDependency(val stepId: String, val requiredStatus: StepStatus = StepStatus.SUCCEEDED)

data class RetryPolicy(
    val maxAttempts: Int = 1,
    val retryFailed: Boolean = true,
    val retryLost: Boolean = true
) {
    init { require(maxAttempts >= 1) { "RetryPolicy.maxAttempts must be at least one" } }

    fun allows(status: AttemptStatus, attemptsSoFar: Int): Boolean = attemptsSoFar < maxAttempts && when (status) {
        AttemptStatus.FAILED -> retryFailed
        AttemptStatus.LOST -> retryLost
        else -> false
    }
}

data class PlanStep(
    val stepId: String,
    val capability: String,
    val inputs: List<ArtifactReference> = emptyList(),
    val outputs: List<String> = emptyList(),
    val environmentIdentity: String? = null,
    val resourceReference: String? = null,
    val dependencies: List<StepDependency> = emptyList(),
    val retryPolicy: RetryPolicy = RetryPolicy(),
    val executionPayloadReference: String? = null,
    val metadata: Map<String, String> = emptyMap(),
    val provenance: Map<String, String> = emptyMap(),
    val status: StepStatus = StepStatus.PENDING
) {
    init {
        require(stepId.isNotBlank()) { "PlanStep.stepId is required" }
        require(capability.isNotBlank()) { "PlanStep.capability is required" }
        require(dependencies.none { it.stepId == stepId }) { "A PlanStep cannot depend on itself" }
    }

    fun normalizedIdentity(): String = canonicalIdentityJson()
}

data class ExecutionPlan(
    val planId: String,
    val steps: List<PlanStep>,
    val contractVersion: String = CONTROL_PLANE_CONTRACT_VERSION,
    val metadata: Map<String, String> = emptyMap(),
    val provenance: Map<String, String> = emptyMap()
) {
    init {
        require(planId.isNotBlank()) { "ExecutionPlan.planId is required" }
        require(steps.map { it.stepId }.distinct().size == steps.size) { "PlanStep identities must be unique" }
        val known = steps.map { it.stepId }.toSet()
        require(steps.flatMap { it.dependencies }.all { it.stepId in known }) { "ExecutionPlan has missing dependencies" }
        orderedSteps()
    }

    fun step(stepId: String): PlanStep = steps.firstOrNull { it.stepId == stepId }
        ?: throw IllegalArgumentException("Unknown PlanStep: $stepId")

    fun orderedSteps(): List<PlanStep> {
        val remaining = steps.associateBy { it.stepId }.toMutableMap()
        val completed = mutableSetOf<String>()
        val ordered = mutableListOf<PlanStep>()
        while (remaining.isNotEmpty()) {
            val ready = steps.filter { step ->
                step.stepId in remaining && step.dependencies.all { it.stepId in completed }
            }
            require(ready.isNotEmpty()) { "ExecutionPlan dependencies must be acyclic" }
            ready.forEach {
                ordered += it
                completed += it.stepId
                remaining.remove(it.stepId)
            }
        }
        return ordered
    }

    fun canonicalIdentityJson(): String = canonicalObject(
        mapOf(
            "contract_version" to quoteCanonical(contractVersion),
            "steps" to canonicalArray(steps.map { it.canonicalIdentityJson() }),
            "metadata" to canonicalMap(metadata),
            "provenance" to canonicalMap(provenance),
        )
    )

    companion object {
        fun deterministic(
            steps: List<PlanStep>,
            metadata: Map<String, String> = emptyMap(),
            provenance: Map<String, String> = emptyMap(),
            contractVersion: String = CONTROL_PLANE_CONTRACT_VERSION
        ): ExecutionPlan {
            val canonical = ExecutionPlan("fixture-plan", steps, contractVersion, metadata, provenance)
                .canonicalIdentityJson()
            val digest = MessageDigest.getInstance("SHA-256").digest(canonical.toByteArray(Charsets.UTF_8))
                .joinToString("") { "%02x".format(it.toInt() and 0xff) }
            return ExecutionPlan("plan-$digest", steps, contractVersion, metadata, provenance)
        }
    }
}
