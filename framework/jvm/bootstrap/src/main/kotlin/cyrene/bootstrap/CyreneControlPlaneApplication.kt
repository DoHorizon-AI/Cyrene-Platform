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
