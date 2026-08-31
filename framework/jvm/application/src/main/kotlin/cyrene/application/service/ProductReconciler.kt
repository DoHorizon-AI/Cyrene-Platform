// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/jvm/application/src/main/kotlin/cyrene/application/service/ProductReconciler.kt
// ║ Module: CYRENE Platform
// ║ Role: Kotlin Framework implementation or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kotlin Framework 实现或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
package cyrene.application.service

import cyrene.domain.control.AttemptStatus
import cyrene.domain.control.DesiredState
import cyrene.domain.control.ExecutionPlan
import cyrene.domain.control.PlanStatus
import cyrene.domain.control.ProductRun
import cyrene.domain.control.StepStatus

enum class ReconcileActionKind {
    CREATE_ATTEMPT, RETRY_ATTEMPT, WAIT, REQUEST_CANCELLATION,
    FINALIZE_SUCCEEDED, FINALIZE_FAILED, FINALIZE_BLOCKED, FINALIZE_CANCELLED, NOOP
}

data class ReconcileAction(
    val kind: ReconcileActionKind,
    val runId: String,
    val stepId: String? = null,
    val attemptId: String? = null,
    val reason: String = ""
)

/** Pure reconciliation policy. Adapters execute actions; this does not own Kernel semantics. */
class ProductReconciler {
    // ════════════════════════════════════════════════════════════════════════
    // 🔧 FUNCTION: ProductReconciler.nextAction
    //
    //   Computes the next product-level action from desired state and observed
    //   evidence; execution remains in adapters and Kernel semantics stay below.
    //
    //   根据期望状态与观测证据计算下一项产品动作；动作由适配器执行，Kernel 语义
    //   仍由下层拥有。
    // ════════════════════════════════════════════════════════════════════════
    fun nextAction(plan: ExecutionPlan, run: ProductRun): ReconcileAction {
        if (run.terminal) return ReconcileAction(ReconcileActionKind.NOOP, run.runId, reason = "ProductRun is terminal")
        val latest = run.latestAttempt
        if (run.desiredState == DesiredState.CANCELLED) {
            if (latest != null && latest.status in setOf(AttemptStatus.PENDING, AttemptStatus.RUNNING, AttemptStatus.CANCELLING)) {
                return ReconcileAction(ReconcileActionKind.REQUEST_CANCELLATION, run.runId, latest.stepId, latest.attemptId)
            }
            if (latest?.status == AttemptStatus.CANCELLED && !latest.cleanupConfirmed) {
                return ReconcileAction(ReconcileActionKind.WAIT, run.runId, latest.stepId, latest.attemptId, "cleanup remains unconfirmed")
            }
            return ReconcileAction(ReconcileActionKind.FINALIZE_CANCELLED, run.runId)
        }
        if (run.stepStatuses.values.any { it == StepStatus.BLOCKED }) return ReconcileAction(ReconcileActionKind.FINALIZE_BLOCKED, run.runId)
        if (latest != null) {
            if (latest.status in setOf(AttemptStatus.PENDING, AttemptStatus.RUNNING, AttemptStatus.CANCELLING)) {
                return ReconcileAction(ReconcileActionKind.WAIT, run.runId, latest.stepId, latest.attemptId)
            }
            if (latest.status in setOf(AttemptStatus.FAILED, AttemptStatus.LOST)) {
                val step = plan.step(latest.stepId)
                if (step.retryPolicy.allows(latest.status, run.attemptsForStep(latest.stepId).size)) {
                    return ReconcileAction(ReconcileActionKind.RETRY_ATTEMPT, run.runId, latest.stepId, latest.attemptId)
                }
                return ReconcileAction(ReconcileActionKind.FINALIZE_FAILED, run.runId, latest.stepId, latest.attemptId)
            }
            if (latest.status == AttemptStatus.BLOCKED) return ReconcileAction(ReconcileActionKind.FINALIZE_BLOCKED, run.runId, latest.stepId, latest.attemptId)
        }
        val next = plan.orderedSteps().firstOrNull { step ->
            run.stepStatuses.getOrDefault(step.stepId, StepStatus.PENDING) == StepStatus.PENDING &&
                step.dependencies.all { dependency -> run.stepStatuses.getOrDefault(dependency.stepId, StepStatus.PENDING) == dependency.requiredStatus }
        }
        if (next != null) return ReconcileAction(ReconcileActionKind.CREATE_ATTEMPT, run.runId, next.stepId)
        if (plan.steps.all { run.stepStatuses[it.stepId] == StepStatus.SUCCEEDED }) return ReconcileAction(ReconcileActionKind.FINALIZE_SUCCEEDED, run.runId)
        return ReconcileAction(ReconcileActionKind.WAIT, run.runId)
    }
}
