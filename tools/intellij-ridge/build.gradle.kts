plugins {
    id("java")
    id("org.jetbrains.kotlin.jvm") version "2.0.21"
    id("org.jetbrains.intellij.platform") version "2.11.0"
}

group = "lang.ridge"
version = "0.1.0"

repositories {
    mavenCentral()
    intellijPlatform {
        defaultRepositories()
        intellijDependencies()
    }
}

dependencies {
    intellijPlatform {
        // Build against the lowest platform the plugin supports: 2024.2, the
        // floor required by LSP4IJ. Together with the open untilBuild below this
        // gives the widest range — every IDE from 2024.2 through the latest
        // release runs the plugin, Community and paid alike.
        intellijIdeaCommunity("2024.2")

        // Latest LSP4IJ. It tracks the newest IDEs (2025.3, 2026.1) and keeps a
        // 242 floor. The <depends> in plugin.xml makes the Marketplace install it
        // for users automatically.
        plugin("com.redhat.devtools.lsp4ij", "0.20.1")

        // Bundled in every JetBrains IDE; provides the TextMate engine that
        // renders the single-sourced Ridge grammar.
        bundledPlugin("org.jetbrains.plugins.textmate")

        pluginVerifier()
        zipSigner()
    }
}

intellijPlatform {
    pluginConfiguration {
        ideaVersion {
            sinceBuild = "242"
            // No upper bound: the plugin uses only long-stable platform API plus
            // LSP4IJ, so it keeps working on future IDE releases.
            untilBuild = provider { null }
        }
    }

    signing {
        certificateChain = providers.environmentVariable("CERTIFICATE_CHAIN")
        privateKey = providers.environmentVariable("PRIVATE_KEY")
        password = providers.environmentVariable("PRIVATE_KEY_PASSWORD")
    }

    publishing {
        token = providers.environmentVariable("PUBLISH_TOKEN")
    }

    pluginVerification {
        ides {
            // The JetBrains-recommended set spans the 2024.2 floor through the
            // latest release, so binary compatibility is checked against the
            // newest IDEs without pinning versions that go stale. (Verified
            // Compatible against IC-242 and IU-261 during development.)
            recommended()
        }
    }
}

kotlin {
    // 2024.2+ is compiled at Java 21; Gradle provisions a JDK 21 toolchain
    // (see settings.gradle.kts) even when the host only has an older JDK.
    jvmToolchain(21)
}

// Single-source the TextMate grammar from the VS Code extension so both editors
// share one ridge.tmLanguage.json. The files land in the plugin jar under
// /textmate and are unpacked at runtime by RidgeTextMateBundleProvider.
val syncTextMateGrammar = tasks.register<Copy>("syncTextMateGrammar") {
    val vscode = layout.projectDirectory.dir("../vscode-ridge")
    into(layout.buildDirectory.dir("generated/textmate/textmate"))
    from(vscode.file("language-configuration.json"))
    from(vscode.file("syntaxes/ridge.tmLanguage.json")) { into("syntaxes") }
}

sourceSets.named("main") {
    resources.srcDir(layout.buildDirectory.dir("generated/textmate"))
}

tasks.named("processResources") {
    dependsOn(syncTextMateGrammar)
}
