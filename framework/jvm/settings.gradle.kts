pluginManagement {
    plugins {
        id("org.gradle.toolchains.foojay-resolver-convention") version "0.8.0"
    }
}

plugins {
    id("org.gradle.toolchains.foojay-resolver-convention")
}

rootProject.name = "cyrene-control-plane"

include("domain")
include("application")
include("adapters:inbound-grpc")
include("adapters:outbound-kernel")
include("bootstrap")
include("architecture-tests")
