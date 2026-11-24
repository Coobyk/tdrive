import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
}

android {
    namespace = "dev.tdrive.app"
    compileSdk = 37

    defaultConfig {
        applicationId = "dev.tdrive.app"
        minSdk = 26
        targetSdk = 37
        versionCode = 302
        versionName = "0.3.2"
    }

    signingConfigs {
        val propsFile = rootProject.file("local.properties")
        if (propsFile.exists()) {
            create("release") {
                val props = Properties().apply { load(propsFile.inputStream()) }
                storeFile = rootProject.file(props.getProperty("tdrive.storeFile"))
                storePassword = props.getProperty("tdrive.storePassword")
                keyAlias = props.getProperty("tdrive.keyAlias")
                keyPassword = props.getProperty("tdrive.keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        compose = true
    }
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2026.09.00"))
    implementation("androidx.compose.material3:material3:1.5.0-beta02")
    implementation("androidx.compose.material:material-icons-extended:1.7.8")
    implementation("androidx.core:core-ktx:1.16.0")
    implementation("androidx.documentfile:documentfile:1.1.0")
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.9.1")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.9.1")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.9.0")
}
