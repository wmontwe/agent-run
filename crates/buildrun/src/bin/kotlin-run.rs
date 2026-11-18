//! Run the project's Kotlin Toolchain wrapper in its build container.

fn main() -> std::process::ExitCode {
    buildrun::client::main(buildrun::Tool::Kotlin)
}
