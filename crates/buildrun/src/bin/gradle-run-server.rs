//! The build container's command: serves `gradle-run` on the pod's loopback.

use buildrun::DEFAULT_PORT;
use buildrun::server::{Config, Queue, handle};
use std::net::TcpListener;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

fn main() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => Arc::new(config),
        Err(e) => {
            eprintln!("gradle-run-server: {e}");
            return ExitCode::FAILURE;
        }
    };
    let port = std::env::var("AGENT_RUN_BUILD_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    // Loopback only: the pod's containers share it, nothing else reaches it.
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("gradle-run-server: 127.0.0.1:{port}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let queue = Arc::new(Queue::new(config.max_concurrent));
    let installs = Arc::new(Mutex::new(()));
    eprintln!("gradle-run-server: listening on 127.0.0.1:{port}");
    for stream in listener.incoming().flatten() {
        let (config, queue, installs) = (config.clone(), queue.clone(), installs.clone());
        std::thread::spawn(move || handle(stream, &config, &queue, &installs));
    }
    ExitCode::SUCCESS
}
