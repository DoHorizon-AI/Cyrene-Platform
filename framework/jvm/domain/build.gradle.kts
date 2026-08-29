plugins {
    kotlin("jvm")
}

dependencies {
    // Pure Kotlin Domain: Strictly ZERO Spring / Jakarta / gRPC dependencies!
    testImplementation(kotlin("test"))
}
