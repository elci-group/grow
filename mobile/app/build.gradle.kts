plugins {
    id("com.android.application")
    kotlin("android")
}

android {
    namespace = "dev.grow.browser"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.grow.browser"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    sourceSets {
        // Rust JNI libraries land here via:
        // cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 \
        //   -o mobile/app/src/main/jniLibs build --release -p grow-mobile
        named("main") {
            jniLibs.srcDir("src/main/jniLibs")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlin {
        compilerOptions {
            jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
        }
    }
}

dependencies {
    // No AndroidX: the browser UI is plain framework views over the JNI engine.
    implementation("org.jetbrains.kotlin:kotlin-stdlib:2.2.20")
}
