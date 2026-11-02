import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.android.application)
}

android {
    namespace = "me.vorotnikov.nsa5gfix"
    compileSdk = 37
    buildToolsVersion = "37.0.0"
    defaultConfig {
        applicationId = "me.vorotnikov.nsa5gfix"
        minSdk = 26
        targetSdk = 37
        versionCode = 1
        versionName = "1.0"
    }
    signingConfigs {
        create("sideload") {
            storeFile = file("../debug.keystore")
            storePassword = "nsa5gfix"
            keyAlias = "nsa5gfix"
            keyPassword = "nsa5gfix"
        }
    }
    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("sideload")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    packaging {
        // Extract the daemon as a real file under nativeLibraryDir so the
        // root child can stage and exec it.
        jniLibs { useLegacyPackaging = true }
    }
}

kotlin { compilerOptions { jvmTarget.set(JvmTarget.JVM_17) } }
