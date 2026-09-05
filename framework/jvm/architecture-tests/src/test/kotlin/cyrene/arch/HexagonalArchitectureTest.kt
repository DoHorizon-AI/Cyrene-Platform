// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/jvm/architecture-tests/src/test/kotlin/cyrene/arch/HexagonalArchitectureTest.kt
// ║ Module: CYRENE Platform
// ║ Role: Kotlin Framework implementation or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kotlin Framework 实现或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
package cyrene.arch

import com.tngtech.archunit.core.importer.ClassFileImporter
import com.tngtech.archunit.lang.syntax.ArchRuleDefinition.noClasses
import cyrene.domain.control.ProductRun
import java.nio.file.Files
import java.nio.file.Path
import java.util.jar.JarFile
import org.junit.jupiter.api.Test
import kotlin.test.assertTrue

/** Architectural gate that must inspect real compiled domain classes. */
class HexagonalArchitectureTest {

    @Test
    fun rule1_domain_layer_must_have_zero_spring_or_grpc_dependencies() {
        val domainLocation = requireNotNull(ProductRun::class.java.protectionDomain.codeSource?.location) {
            "compiled domain code location is unavailable"
        }
        val domainPath = Path.of(domainLocation.toURI())
        val importer = ClassFileImporter()
        val domainClasses = if (Files.isDirectory(domainPath)) {
            importer.importPath(domainPath)
        } else {
            JarFile(domainPath.toFile()).use(importer::importJar)
        }
        assertTrue(
            domainClasses.any { it.packageName.startsWith("cyrene.domain") },
            "architecture gate must inspect compiled domain classes from $domainLocation; " +
                "imported=${domainClasses.joinToString { it.name }}"
        )
        noClasses()
            .that().resideInAPackage("cyrene.domain..")
            .should().dependOnClassesThat().resideInAnyPackage(
                "org.springframework..",
                "jakarta..",
                "javax..",
                "io.grpc..",
                "com.google.protobuf.."
            )
            .check(domainClasses)
    }

}
