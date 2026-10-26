plugins {
    kotlin("jvm")
    kotlin("plugin.serialization")
}
kotlin { jvmToolchain(17) }

// The group-crypto adapter is shared with the app. Here it is compiled for the JVM tests against the
// generated UniFFI binding and a host build of crates/pigeonpost-groupcrypto (target/release), so
// the client loop runs real MLS: `cargo build --release -p pigeonpost-groupcrypto-ffi` in that crate.
val groupCrypto = rootDir.resolve("../../crates/pigeonpost-groupcrypto")
sourceSets.named("test") {
    kotlin.srcDir("src/groupcrypto/kotlin")
    kotlin.srcDir("src/groupcrypto-fake/kotlin")
    kotlin.srcDir(groupCrypto.resolve("bindings/kotlin"))
}
tasks.withType<Test>().configureEach {
    systemProperty("jna.library.path", groupCrypto.resolve("target/release").absolutePath)
    // CI sets this so a missing host library fails the build instead of skipping the MLS tests.
    providers.environmentVariable("GROUPCRYPTO_REQUIRED").orNull?.let { systemProperty("groupcrypto.required", it) }
}

// Opt-in real HTTP qualification; ordinary JVM test runs need no postbox or fixture credentials.
tasks.register<JavaExec>("messageActionsPostbox") {
    group = "verification"
    description = "Checks message actions against a disposable loopback postbox."
    dependsOn("testClasses")
    classpath = sourceSets.named("test").get().runtimeClasspath
    mainClass.set("dev.pigeonpost.core.MessageActionsPostbox")
    doFirst {
        val fixture = providers.environmentVariable("PIGEONPOST_TEST_FIXTURE").orNull
        require(!fixture.isNullOrBlank()) { "Set PIGEONPOST_TEST_FIXTURE to the private loopback fixture JSON." }
        setArgs(listOf(fixture))
    }
}

dependencies {
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.9.0")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    testImplementation("junit:junit:4.13.2")
    testImplementation("com.squareup.okhttp3:mockwebserver:4.12.0")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.10.2")
    testImplementation("net.java.dev.jna:jna:5.17.0")
}
