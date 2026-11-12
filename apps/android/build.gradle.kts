// The versions are in gradle/libs.versions.toml. Kotlin is built into the
// Android Gradle plugin (AGP 9); `kotlin-android` is listed only to put our
// Kotlin version on the build's classpath.
plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.android.library) apply false
    alias(libs.plugins.kotlin.android) apply false
    alias(libs.plugins.kotlin.compose) apply false
    alias(libs.plugins.kotlin.serialization) apply false
    alias(libs.plugins.openapi.generator) apply false
    alias(libs.plugins.ktlint) apply false
}

val ktlintVersion = libs.versions.ktlint.cli.get()

subprojects {
    apply(plugin = "org.jlleitschuh.gradle.ktlint")
    configure<org.jlleitschuh.gradle.ktlint.KtlintExtension> {
        version.set(ktlintVersion)
        filter {
            // The generated client is not ours to format.
            exclude { it.file.path.contains("${File.separator}build${File.separator}") }
        }
    }
}
