package cyrene.application.service

import cyrene.domain.control.Attempt
import cyrene.domain.control.AttemptStatus
import cyrene.domain.control.DesiredState
import cyrene.domain.control.ExecutionPlan
import cyrene.domain.control.PlanStep
import cyrene.domain.control.ProductRun
import cyrene.domain.control.RetryPolicy
import cyrene.domain.control.StepDependency
import cyrene.domain.control.StepStatus
import kotlin.test.Test
import kotlin.test.assertEquals

class ProductReconcilerTest {
    private val plan = ExecutionPlan.deterministic(listOf(
        PlanStep("prepare", "capability.prepare"),
        PlanStep("execute", "capability.execute", dependencies = listOf(StepDependency("prepare")))
    ))

    @Test fun `dependency ordering creates only the first ready step`() {
        val action = ProductReconciler().nextAction(plan, ProductRun("run-1", "example", plan.planId, "key-1"))
        assertEquals(ReconcileActionKind.CREATE_ATTEMPT, action.kind)
        assertEquals("prepare", action.stepId)
    }

    @Test fun `lost attempt with capacity gets a new attempt`() {
        val retryPlan = ExecutionPlan.deterministic(listOf(PlanStep("execute", "capability.execute", retryPolicy = RetryPolicy(maxAttempts = 2))))
        val run = ProductRun("run-1", "example", retryPlan.planId, "key-1", attempts = listOf(Attempt("run-1:1", "run-1", 1, "execute", AttemptStatus.LOST)))
        assertEquals(ReconcileActionKind.RETRY_ATTEMPT, ProductReconciler().nextAction(retryPlan, run).kind)
    }

    @Test fun `cancelled acknowledgement waits until cleanup is confirmed`() {
        val run = ProductRun("run-1", "example", plan.planId, "key-1", desiredState = DesiredState.CANCELLED, attempts = listOf(Attempt("run-1:1", "run-1", 1, "prepare", AttemptStatus.CANCELLED, cleanupConfirmed = false)))
        assertEquals(ReconcileActionKind.WAIT, ProductReconciler().nextAction(plan, run).kind)
    }

    @Test fun `all successful steps finalize product success`() {
        val run = ProductRun("run-1", "example", plan.planId, "key-1", stepStatuses = mapOf("prepare" to StepStatus.SUCCEEDED, "execute" to StepStatus.SUCCEEDED))
        assertEquals(ReconcileActionKind.FINALIZE_SUCCEEDED, ProductReconciler().nextAction(plan, run).kind)
    }
}
