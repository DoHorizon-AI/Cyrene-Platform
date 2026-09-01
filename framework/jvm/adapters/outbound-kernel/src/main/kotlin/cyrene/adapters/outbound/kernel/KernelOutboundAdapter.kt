// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/jvm/adapters/outbound-kernel/src/main/kotlin/cyrene/adapters/outbound/kernel/KernelOutboundAdapter.kt
// ║ Module: CYRENE Platform
// ║ Role: Kotlin Framework implementation or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kotlin Framework 实现或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
package cyrene.adapters.outbound.kernel

import cyrene.application.port.outbound.KernelCommandEnvelope
import cyrene.application.port.outbound.KernelCommandOutcome
import cyrene.application.port.outbound.KernelCommandPort

/**
 * Outbound Kernel adapter enforcing platform-component-boundary-v1 §4.2 invariants:
 * Every state-changing call to Kernel MUST carry ContractRevision, Request ID, Idempotency Key,
 * and transport-injected Principal.
 */
class KernelOutboundAdapter : KernelCommandPort {

    override fun executeCommand(envelope: KernelCommandEnvelope): KernelCommandOutcome {
        require(envelope.contractRevision.isNotBlank()) {
            "Kernel command rejected: missing selected ContractRevision"
        }
        require(envelope.requestId.isNotBlank()) {
            "Kernel command rejected: missing Request ID"
        }
        require(envelope.idempotencyKey.isNotBlank()) {
            "Kernel command rejected: missing Idempotency Key"
        }
        require(envelope.principalUid >= 0) {
            "Kernel command rejected: missing or invalid transport-injected Principal"
        }

        // In local node mock / simulated client:
        return KernelCommandOutcome(
            commandId = envelope.commandId,
            success = true,
            responseBytes = byteArrayOf()
        )
    }
}
