// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/jvm/adapters/inbound-grpc/src/main/kotlin/cyrene/adapters/inbound/grpc/NodeControlGrpcServer.kt
// ║ Module: CYRENE Platform
// ║ Role: Kotlin Framework implementation or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kotlin Framework 实现或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
package cyrene.adapters.inbound.grpc

import io.grpc.Server
import io.grpc.netty.shaded.io.grpc.netty.GrpcSslContexts
import io.grpc.netty.shaded.io.grpc.netty.NettyServerBuilder
import io.grpc.netty.shaded.io.netty.handler.ssl.ClientAuth
import io.grpc.netty.shaded.io.netty.handler.ssl.SslContext
import java.io.File
import java.net.InetSocketAddress
import java.util.concurrent.TimeUnit

/**
 * Production gRPC Server Host for NodeControlService over mTLS.
 * Handles server lifecycle (bind, start, shutdown) and TLS certificates per node-agent specification.
 */
class NodeControlGrpcServer(
    val bindHost: String = "127.0.0.1",
    val port: Int = 50051,
    val nodeControlService: NodeControlService = NodeControlService(),
    val certChainFile: File? = null,
    val privateKeyFile: File? = null,
    val trustCertCollectionFile: File? = null
) : AutoCloseable {
    private var server: Server? = null

    @Synchronized
    fun start(): NodeControlGrpcServer {
        if (server != null && !server!!.isShutdown) {
            return this
        }

        val builder = NettyServerBuilder.forAddress(InetSocketAddress(bindHost, port))
            .addService(nodeControlService)

        // Real mTLS Dual-Certificate assembly
        if (certChainFile != null && privateKeyFile != null) {
            val sslContext: SslContext = GrpcSslContexts.forServer(certChainFile, privateKeyFile)
                .trustManager(trustCertCollectionFile)
                .clientAuth(ClientAuth.REQUIRE)
                .build()
            builder.sslContext(sslContext)
        }

        server = builder.build().start()
        return this
    }

    @Synchronized
    fun stop(timeoutSeconds: Long = 5) {
        server?.let {
            it.shutdown()
            try {
                if (!it.awaitTermination(timeoutSeconds, TimeUnit.SECONDS)) {
                    it.shutdownNow()
                }
            } catch (e: InterruptedException) {
                it.shutdownNow()
            }
            server = null
        }
    }

    fun isRunning(): Boolean = server != null && !server!!.isShutdown && !server!!.isTerminated

    override fun close() {
        stop()
    }
}
