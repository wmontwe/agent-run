// agent-run: points every repository with a known remote URL at the mirror, in every build
// of the composite. Content filters stay as declared. Unknown
// remotes are left alone: egress refuses them, so the build fails visibly.
val mirror = System.getenv("AGENT_RUN_MIRROR") ?: throw GradleException("AGENT_RUN_MIRROR is not set")
// "id=url,id=url", from the policy's mirror.remotes.
val remotes = (System.getenv("AGENT_RUN_MIRROR_REMOTES") ?: "")
    .split(',').filter { it.isNotBlank() }
    .associate { entry -> entry.substringAfter('=').trimEnd('/') to entry.substringBefore('=') }
// Other spellings of the same repositories.
val synonyms = mapOf(
    "https://repo1.maven.org/maven2" to "https://repo.maven.apache.org/maven2",
    "https://maven.google.com" to "https://dl.google.com/dl/android/maven2",
)

fun redirect(repo: ArtifactRepository) {
    val url = when (repo) {
        is MavenArtifactRepository -> repo.url
        is IvyArtifactRepository -> repo.url
        else -> return
    }
    if (url.scheme == "file") return
    val key = url.toString().trimEnd('/').let { synonyms[it] ?: it }
    val id = remotes[key] ?: return logger.warn("agent-run: no mirror for ${repo.name} ($key)")
    val target = java.net.URI("$mirror/$id/")
    when (repo) {
        is MavenArtifactRepository -> { repo.setUrl(target); repo.isAllowInsecureProtocol = true }
        is IvyArtifactRepository -> { repo.setUrl(target); repo.isAllowInsecureProtocol = true }
    }
}

beforeSettings {
    pluginManagement.repositories.all { redirect(this) }
    dependencyResolutionManagement.repositories.all { redirect(this) }
}
// Not allprojects: Isolated Projects forbids it.
gradle.beforeProject {
    buildscript.repositories.all { redirect(this) }
    repositories.all { redirect(this) }
}
