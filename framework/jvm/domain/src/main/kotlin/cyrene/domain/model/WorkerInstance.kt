// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/jvm/domain/src/main/kotlin/cyrene/domain/model/WorkerInstance.kt
// ║ Module: CYRENE Platform
// ║ Role: Kotlin Framework implementation or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kotlin Framework 实现或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
package cyrene.domain.model

import java.time.Duration
import java.time.Instant

/**
 * Pure Kotlin domain model representing a running Worker instance.
 */
data class WorkerInstance(
    val instanceId: String,
    val nodeId: String,
    val pluginId: String,
    val state: WorkerState,
    val heartbeatInterval: Duration,
    val lastHeartbeatAt: Instant,
    val crashCount: Int = 0
) {
    fun recordHeartbeat(now: Instant): WorkerInstance =
        copy(lastHeartbeatAt = now, state = WorkerState.HEALTHY)

    fun recordCrash(): WorkerInstance {
        val newCrashes = crashCount + 1
        return if (newCrashes > 3) {
            copy(crashCount = newCrashes, state = WorkerState.QUARANTINED)
        } else {
            copy(crashCount = newCrashes, state = WorkerState.STOPPED)
        }
    }
}

enum class WorkerState {
    STARTING,
    HEALTHY,
    DEGRADED,
    DRAINING,
    STOPPED,
    QUARANTINED
}
