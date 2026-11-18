//! Run Cargo in the project's build container, with container-owned state.

fn main() -> std::process::ExitCode {
    buildrun::client::main(buildrun::Tool::Cargo)
}
