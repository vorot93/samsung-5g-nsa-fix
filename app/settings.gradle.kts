pluginManagement {
    repositories {
        // repo.maven.apache.org is 403-blocked on this network; the Google
        // mirror serves the same Maven Central artifacts.
        maven { url = uri("https://maven-central.storage-download.googleapis.com/maven2/") }
        google(); mavenCentral(); gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositories {
        maven { url = uri("https://maven-central.storage-download.googleapis.com/maven2/") }
        google(); mavenCentral()
    }
}
rootProject.name = "samsung-5g-nsa-fix"
include(":app")
