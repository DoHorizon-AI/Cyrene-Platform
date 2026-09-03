// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/jvm/application/src/main/kotlin/cyrene/application/port/outbound/KernelCommandPort.kt
// ║ Module: CYRENE Platform
// ║ Role: Kotlin Framework implementation or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kotlin Framework 实现或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
package cyrene.application.port.outbound

data class KernelCommandEnvelope(
    val commandId: String,
    val requestId: String,
    val idempotencyKey: String,
    val contractRevision: String,
    val principalUid: Long,
    val principalGid: Long,
    val actionName: String,
    val payloadBytes: ByteArray
)

data class KernelCommandOutcome(
    val commandId: String,
    val success: Boolean,
    val responseBytes: ByteArray,
    val errorCode: String? = null,
    val errorMessage: String? = null
)

interface KernelCommandPort {
    fun executeCommand(envelope: KernelCommandEnvelope): KernelCommandOutcome
}
