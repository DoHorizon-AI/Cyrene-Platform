package cyrene.domain.control

import java.time.Instant

typealias AttemptId = String
typealias AttemptNumber = Int
typealias Generation = Long
typealias IdempotencyKey = String

enum class DesiredState { ACTIVE, CANCELLED }

enum class AttemptStatus {
    PENDING, RUNNING, SUCCEEDED, FAILED, LOST, BLOCKED, CANCELLING, CANCELLED;

    val terminal: Boolean
        get() = this in setOf(SUCCEEDED, FAILED, LOST, BLOCKED, CANCELLED)
}

data class Attempt(
    val attemptId: AttemptId,
    val runId: String,
    val number: AttemptNumber,
    val stepId: String,
    val status: AttemptStatus = AttemptStatus.PENDING,
    val executionReferences: List<String> = emptyList(),
    val cleanupConfirmed: Boolean = false,
    val error: String? = null,
    val metadata: Map<String, String> = emptyMap(),
    val createdAt: Instant = Instant.now(),
    val observedAt: Instant = Instant.now()
) {
    fun observe(
        nextStatus: AttemptStatus,
        executionReferences: List<String> = this.executionReferences,
        cleanupConfirmed: Boolean = this.cleanupConfirmed,
        error: String? = this.error,
        now: Instant = Instant.now()
    ): Attempt {
        require(!status.terminal || nextStatus == status) { "Terminal Attempt $attemptId is immutable" }
        return copy(status = nextStatus, executionReferences = executionReferences, cleanupConfirmed = cleanupConfirmed, error = error, observedAt = now)
    }
}

data class ProductRun(
    val runId: String,
    val productKind: String,
    val planId: String,
    val idempotencyKey: IdempotencyKey,
    val desiredState: DesiredState = DesiredState.ACTIVE,
    val observedStatus: PlanStatus = PlanStatus.PENDING,
    val generation: Generation = 0,
    val attempts: List<Attempt> = emptyList(),
    val stepStatuses: Map<String, StepStatus> = emptyMap(),
    val metadata: Map<String, String> = emptyMap(),
    val createdAt: Instant = Instant.now(),
    val updatedAt: Instant = Instant.now()
) {
    val terminal: Boolean
        get() = observedStatus in setOf(PlanStatus.SUCCEEDED, PlanStatus.FAILED, PlanStatus.BLOCKED, PlanStatus.CANCELLED)
    val latestAttempt: Attempt?
        get() = attempts.lastOrNull()
    fun attemptsForStep(stepId: String): List<Attempt> = attempts.filter { it.stepId == stepId }
}
