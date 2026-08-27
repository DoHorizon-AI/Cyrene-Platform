plugins {
    kotlin("jvm") version "2.4.10" apply false
    id("org.springframework.boot") version "3.3.3" apply false
    id("io.spring.dependency-management") version "1.1.6" apply false
    id("com.google.protobuf") version "0.9.4" apply false
}

allprojects {
    group = "com.cyrene.platform"
    version = "0.1.0-SNAPSHOT"

    repositories {
        mavenCentral()
    }
}

subprojects {
    apply(plugin = "org.jetbrains.kotlin.jvm")

    kotlin {
        jvmToolchain(25)
    }

    tasks.withType<Test> {
        useJUnitPlatform()
    }
}
