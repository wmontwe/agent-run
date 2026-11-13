//! `gradle-run <tasks and options>`: runs the project's Gradle wrapper in the project's build
//! container, as if `./gradlew` ran here. Ctrl+C stops the build.

use buildrun::{DEFAULT_PORT, Frame, Request, read_frame, write_request};
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1)),
        Err(message) => {
            eprintln!("gradle-run: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<i32, String> {
    let port = std::env::var("AGENT_RUN_BUILD_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| {
        format!("the project's build container doesn't answer on 127.0.0.1:{port}: {e}")
    })?;
    let cwd = std::env::current_dir().map_err(|e| format!("current folder: {e}"))?;
    let request = Request {
        cwd: cwd
            .to_str()
            .ok_or("the current folder's name isn't UTF-8")?
            .to_owned(),
        display: std::env::var("DISPLAY").ok(),
        args: std::env::args().skip(1).collect(),
    };
    write_request(&mut stream, &request).map_err(|e| format!("sending the request: {e}"))?;
    let mut input = BufReader::new(stream);
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    loop {
        match read_frame(&mut input).map_err(|e| format!("reading the build's output: {e}"))? {
            Some(Frame::Out(bytes)) => {
                let _ = out.write_all(&bytes).and_then(|()| out.flush());
            }
            Some(Frame::Err(bytes)) => {
                let _ = err.write_all(&bytes);
            }
            Some(Frame::Exit(code)) => return Ok(code),
            None => {
                return Err(
                    "the build container closed the connection before the build ended".into(),
                );
            }
        }
    }
}
