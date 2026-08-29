plugins {
    kotlin("jvm")
}

dependencies {
    implementation(project(":domain"))
    implementation(project(":application"))

    // Production gRPC Runtime & Netty Shaded Engine
    implementation("io.grpc:grpc-netty-shaded:1.66.0")
    implementation("io.grpc:grpc-protobuf:1.66.0")
    implementation("io.grpc:grpc-stub:1.66.0")
    implementation("com.google.protobuf:protobuf-kotlin:3.25.3")

    testImplementation(kotlin("test"))
}
