plugins {
    // Lets Gradle download a matching JDK toolchain (21) when one is not already
    // installed, so the build is reproducible regardless of the machine's JDKs.
    id("org.gradle.toolchains.foojay-resolver-convention") version "0.8.0"
}

rootProject.name = "intellij-ridge"
