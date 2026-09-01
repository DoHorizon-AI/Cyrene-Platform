// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/jvm/bootstrap/src/main/kotlin/cyrene/bootstrap/CyreneControlPlaneApplication.kt
// ║ Module: CYRENE Platform
// ║ Role: Kotlin Framework implementation or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kotlin Framework 实现或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
package cyrene.bootstrap

import cyrene.adapters.inbound.grpc.NodeControlService
import cyrene.adapters.outbound.kernel.KernelOutboundAdapter

class CyreneControlPlaneApplication {
    val nodeControlService = NodeControlService()
    val kernelOutboundAdapter = KernelOutboundAdapter()
}

fun main() {
    println("CYRENE JVM Control Plane Initialized.")
}
