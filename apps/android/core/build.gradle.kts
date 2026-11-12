// :core: the API client generated from the backend's OpenAPI spec, the
// session and its token, our own resumable-upload client and the live
// events. No UI.
plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.openapi.generator)
}

// Written by `mise run gen:spec`; never committed, as the web's SDK.
val spec = rootProject.file("../backend/dist/openapi.json")
val generated = layout.buildDirectory.dir("generated/openapi")

openApiGenerate {
    generatorName.set("kotlin")
    library.set("jvm-okhttp4")
    inputSpec.set(spec.absolutePath)
    outputDir.set(generated.get().asFile.absolutePath)
    packageName.set("eu.vardalabs.organizer.api")
    configOptions.putAll(
        mapOf(
            "serializationLibrary" to "kotlinx_serialization",
            "dateLibrary" to "string",
            "sourceFolder" to "src/main/kotlin",
            "enumPropertyNaming" to "UPPERCASE",
            "omitGradleWrapper" to "true",
            "omitGradlePluginVersions" to "true",
        ),
    )
    // The spec's license has neither an SPDX identifier (Hippocratic 3.0
    // has none) nor a URL, which the validator refuses.
    skipValidateSpec.set(true)
    generateApiTests.set(false)
    generateModelTests.set(false)
    generateApiDocumentation.set(false)
    generateModelDocumentation.set(false)
}

tasks.named("openApiGenerate") {
    inputs.file(spec)
}

android {
    namespace = "eu.vardalabs.organizer.core"
    compileSdk = 37

    defaultConfig {
        minSdk = 29
        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    testOptions {
        unitTests.isReturnDefaultValues = true
        unitTests.all {
            // The encryption format's test vectors, read where the reference writes them.
            val vectors = rootProject.file("../e2e-crypto/vectors.json")
            it.systemProperty("e2e.vectors", vectors.absolutePath)
            it.inputs.file(vectors)
        }
    }
}

// The generated client is compiled with our sources (made before every build).
androidComponents {
    onVariants { variant ->
        variant.sources.kotlin?.addStaticSourceDirectory(
            generated
                .get()
                .dir("src/main/kotlin")
                .asFile.path,
        )
    }
}

tasks.named("preBuild") {
    dependsOn("openApiGenerate")
}

dependencies {
    api(libs.okhttp)
    api(libs.kotlinx.serialization.json)
    api(libs.kotlinx.coroutines.android)
    implementation(libs.okhttp.sse)
    api(libs.androidx.datastore.preferences)
    implementation(libs.tink)

    testImplementation(libs.junit)
    testImplementation(libs.okhttp.mockwebserver)
    testImplementation(libs.kotlinx.coroutines.test)
}
