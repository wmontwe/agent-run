//! Run the project's Gradle wrapper in its build container.

fn main() -> std::process::ExitCode {
    buildrun::client::main(buildrun::Tool::Gradle)
}
