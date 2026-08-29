plugins {
    kotlin("jvm")
}

dependencies {
    implementation(project(":domain"))
    implementation(project(":application"))
    implementation(project(":adapters:inbound-grpc"))
    implementation(project(":adapters:outbound-kernel"))
    testImplementation(kotlin("test"))
}
