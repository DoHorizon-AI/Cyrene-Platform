plugins {
    kotlin("jvm")
}

dependencies {
    implementation(project(":domain"))
    implementation(project(":application"))
    testImplementation(kotlin("test"))
}
