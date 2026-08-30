plugins {
    kotlin("jvm") version "2.4.10"
    id("com.google.protobuf") version "0.9.4"
}

repositories {
    mavenCentral()
}

dependencies {
    implementation("com.google.protobuf:protobuf-java:3.25.3")
    implementation("com.google.protobuf:protobuf-kotlin:3.25.3")
    testImplementation(kotlin("test"))
}

kotlin {
    jvmToolchain(25)
}

sourceSets {
    main {
        proto {
            srcDir("../../../contracts/proto")
            include("cyrene/capability/v1/capability_execution.proto")
            include("cyrene/model/provider/v1/model_provider.proto")
        }
    }
}

protobuf {
    protoc {
        artifact = "com.google.protobuf:protoc:3.25.3"
    }
    generateProtoTasks {
        all().configureEach {
            builtins {
                create("kotlin")
            }
        }
    }
}

tasks.test {
    useJUnitPlatform()
}
