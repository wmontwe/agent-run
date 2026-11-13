// agent-run: keeps build state out of the checkout. Every
// project, in every build of the composite, gets its build folder in the build state volume,
// keyed by its path in the checkout. gradle-run sets --project-cache-dir and
// kotlin.project.persistent.dir, which move .gradle and .kotlin.
val workspace = File(System.getenv("AGENT_RUN_WORKSPACE") ?: throw GradleException("AGENT_RUN_WORKSPACE is not set"))
val buildStateRoot = File(System.getenv("AGENT_RUN_BUILD_STATE") ?: throw GradleException("AGENT_RUN_BUILD_STATE is not set"))
    .resolve("projects")

// Not allprojects: Isolated Projects forbids it.
gradle.beforeProject {
    val relative = projectDir.relativeTo(workspace).path.ifEmpty { "." }
    layout.buildDirectory.set(buildStateRoot.resolve(relative).resolve("build"))
}
