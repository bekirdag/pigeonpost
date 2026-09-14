plugins {
    id("com.android.application")
    kotlin("android")
    kotlin("plugin.serialization")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    val firebaseApiKey = providers.environmentVariable("PIGEONPOST_FIREBASE_API_KEY").orNull
    val uploadNames = listOf("ANDROID_UPLOAD_KEYSTORE", "ANDROID_UPLOAD_STORE_PASSWORD", "ANDROID_UPLOAD_KEY_ALIAS", "ANDROID_UPLOAD_KEY_PASSWORD")
    val upload = uploadNames.associateWith { providers.environmentVariable(it).orNull }
    if (upload.values.any { it != null }) {
        require(upload.values.all { !it.isNullOrBlank() }) { "Provide all four ANDROID_UPLOAD signing variables, or leave all unset for an unsigned release build." }
        require(!firebaseApiKey.isNullOrBlank()) { "A signed release requires PIGEONPOST_FIREBASE_API_KEY from the secret store." }
        signingConfigs.create("upload") {
            storeFile = file(upload.getValue("ANDROID_UPLOAD_KEYSTORE")!!)
            storePassword = upload.getValue("ANDROID_UPLOAD_STORE_PASSWORD")
            keyAlias = upload.getValue("ANDROID_UPLOAD_KEY_ALIAS")
            keyPassword = upload.getValue("ANDROID_UPLOAD_KEY_PASSWORD")
        }
    }
    namespace = "dev.pigeonpost.inbox"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.pigeonpost.inbox"
        minSdk = 26
        targetSdk = 36
        versionCode = 8
        versionName = "0.2.5"
        // Firebase project identifiers are public. Inject the restricted API key at build time.
        // Forked PRs without secrets can still run fixtures and produce development artifacts.
        resValue("string", "google_api_key", firebaseApiKey ?: "not-configured")
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        manifestPlaceholders["appAuthRedirectScheme"] = "dev.pigeonpost.inbox"
    }
    buildTypes {
        debug { applicationIdSuffix = ".debug" }
        release {
            signingConfig = signingConfigs.findByName("upload")
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        compose = true
        buildConfig = true
        resValues = true
    }
    packaging { resources.excludes += setOf("META-INF/AL2.0", "META-INF/LGPL2.1") }
    testOptions { unitTests.isReturnDefaultValues = true }
}
kotlin { jvmToolchain(17) }

dependencies {
    implementation(project(":core"))
    val composeBom = platform("androidx.compose:compose-bom:2025.12.01")
    implementation(composeBom)
    androidTestImplementation(composeBom)
    implementation("androidx.core:core-ktx:1.17.0")
    implementation("androidx.activity:activity-compose:1.11.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.9.4")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.9.4")
    implementation("androidx.lifecycle:lifecycle-viewmodel-ktx:2.9.4")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-play-services:1.10.2")
    implementation(platform("com.google.firebase:firebase-bom:34.18.0"))
    implementation("com.google.firebase:firebase-messaging")
    implementation("androidx.work:work-runtime:2.11.2")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.9.0")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation("net.openid:appauth:0.11.1")
    implementation("com.android.billingclient:billing:9.1.0")
    implementation("androidx.browser:browser:1.9.0")
    implementation("com.journeyapps:zxing-android-embedded:4.3.0")
    implementation("org.commonmark:commonmark:0.24.0")
    debugImplementation("androidx.compose.ui:ui-tooling")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.10.2")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test:runner:1.7.0")
    androidTestImplementation("androidx.test:rules:1.7.0")
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
}
