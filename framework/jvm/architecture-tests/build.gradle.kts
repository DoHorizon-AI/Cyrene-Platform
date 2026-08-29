plugins {
    kotlin("jvm")
}

dependencies {
    implementation(project(":domain"))
    implementation(project(":application"))
    implementation(project(":adapters:inbound-grpc"))
    implementation(project(":adapters:outbound-kernel"))
    implementation(project(":bootstrap"))

    testImplementation(kotlin("test"))
    testImplementation("com.tngtech.archunit:archunit-junit5:1.3.0")
}
