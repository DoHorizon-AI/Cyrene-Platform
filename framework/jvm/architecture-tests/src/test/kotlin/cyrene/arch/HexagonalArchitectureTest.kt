// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/jvm/architecture-tests/src/test/kotlin/cyrene/arch/HexagonalArchitectureTest.kt
// ║ Module: CYRENE Platform
// ║ Role: Kotlin Framework implementation or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kotlin Framework 实现或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
package cyrene.arch

import com.tngtech.archunit.core.importer.ImportOption
import com.tngtech.archunit.junit.AnalyzeClasses
import com.tngtech.archunit.junit.ArchTest
import com.tngtech.archunit.lang.ArchRule
import com.tngtech.archunit.lang.syntax.ArchRuleDefinition.noClasses
import cyrene.adapters.outbound.kernel.KernelOutboundAdapter
import cyrene.application.port.outbound.KernelCommandEnvelope
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * Architectural Gate Enforcement Tests (Dual ArchUnit Rules & Boundary Verification).
 */
@AnalyzeClasses(
    packages = ["cyrene"],
    importOptions = [ImportOption.DoNotIncludeTests::class]
)
class HexagonalArchitectureTest {

    @ArchTest
    val rule1_domain_layer_must_have_zero_spring_or_grpc_dependencies: ArchRule =
        noClasses()
            .that().resideInAPackage("cyrene.domain..")
            .should().dependOnClassesThat().resideInAnyPackage(
                "org.springframework..",
                "jakarta..",
                "javax..",
                "io.grpc..",
                "com.google.protobuf.."
            )

    @Test
    fun rule2_outbound_kernel_adapter_must_enforce_contract_and_principal_injection() {
        val adapter = KernelOutboundAdapter()

        // 1. Missing ContractRevision -> Rejection
        assertThrows<IllegalArgumentException> {
            adapter.executeCommand(
                KernelCommandEnvelope(
                    commandId = "cmd-1",
                    requestId = "req-1",
                    idempotencyKey = "idemp-1",
                    contractRevision = "", // missing!
                    principalUid = 1000,
                    principalGid = 1000,
                    actionName = "LaunchPlugin",
                    payloadBytes = byteArrayOf()
                )
            )
        }

        // 2. Missing Principal -> Rejection
        assertThrows<IllegalArgumentException> {
            adapter.executeCommand(
                KernelCommandEnvelope(
                    commandId = "cmd-2",
                    requestId = "req-2",
                    idempotencyKey = "idemp-2",
                    contractRevision = "v1.0",
                    principalUid = -1, // invalid!
                    principalGid = 1000,
                    actionName = "LaunchPlugin",
                    payloadBytes = byteArrayOf()
                )
            )
        }

        // 3. Valid Envelope with all metadata -> Success
        val validOutcome = adapter.executeCommand(
            KernelCommandEnvelope(
                commandId = "cmd-3",
                requestId = "req-3",
                idempotencyKey = "idemp-3",
                contractRevision = "v1.0",
                principalUid = 1000,
                principalGid = 1000,
                actionName = "LaunchPlugin",
                payloadBytes = byteArrayOf()
            )
        )
        assertTrue(validOutcome.success)
        assertEquals("cmd-3", validOutcome.commandId)
    }
}
