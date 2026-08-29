package cyrene.application.port.outbound

import cyrene.domain.control.ExecutionPlan
import cyrene.domain.control.Generation
import cyrene.domain.control.IdempotencyKey
import cyrene.domain.control.ProductRun

/** Persistent boundary; concrete storage remains replaceable and Product-neutral. */
interface ProductControlPlaneStore {
    fun createOrGet(plan: ExecutionPlan, productKind: String, idempotencyKey: IdempotencyKey, metadata: Map<String, String> = emptyMap()): ProductRun
    fun loadRun(runId: String): ProductRun
    fun loadPlan(planId: String): ExecutionPlan
    fun compareAndSet(run: ProductRun, expectedGeneration: Generation): ProductRun
    fun listNonTerminal(): List<ProductRun>
}

class StaleGenerationException(message: String) : RuntimeException(message)
class IdempotencyConflictException(message: String) : RuntimeException(message)
