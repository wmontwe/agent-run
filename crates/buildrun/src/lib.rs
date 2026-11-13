//! `gradle-run`: agents run the project's Gradle
//! wrapper in the project's build container, which listens on the pod's `localhost`.
//!
//! The protocol is line-based. The client sends `gradle-run 1`, then `cwd`, `display` and
//! `arg` fields (`key\tvalue`, values escaped), then `run`. The server answers with frames:
//! `out <n>` or `err <n>` followed by n bytes, and finally `exit <code>`. A client that
//! disconnects cancels its build.

pub mod server;
pub mod wrapper;

use std::io::{self, BufRead, Write};

/// Where the build server listens: the pod's loopback, shared by its containers.
pub const DEFAULT_PORT: u16 = 7300;
const HELLO: &str = "gradle-run 1";

/// What a client asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Request {
    pub cwd: String,
    pub display: Option<String>,
    pub args: Vec<String>,
}

fn escape(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\n', "%0A")
        .replace('\t', "%09")
        .replace('\r', "%0D")
}

fn unescape(value: &str) -> String {
    value
        .replace("%0A", "\n")
        .replace("%09", "\t")
        .replace("%0D", "\r")
        .replace("%25", "%")
}

pub fn write_request(out: &mut impl Write, request: &Request) -> io::Result<()> {
    let mut text = format!("{HELLO}\ncwd\t{}\n", escape(&request.cwd));
    if let Some(display) = &request.display {
        text += &format!("display\t{}\n", escape(display));
    }
    for arg in &request.args {
        text += &format!("arg\t{}\n", escape(arg));
    }
    text += "run\n";
    out.write_all(text.as_bytes())?;
    out.flush()
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub fn read_request(input: &mut impl BufRead) -> io::Result<Request> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    if line.trim_end() != HELLO {
        return Err(invalid(format!(
            "not a gradle-run request: {:?}",
            line.trim_end()
        )));
    }
    let mut request = Request::default();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Err(invalid("request ended early"));
        }
        let line = line.trim_end_matches('\n');
        if line == "run" {
            return Ok(request);
        }
        let (key, value) = line
            .split_once('\t')
            .ok_or_else(|| invalid(format!("bad field: {line:?}")))?;
        let value = unescape(value);
        match key {
            "cwd" => request.cwd = value,
            "display" => request.display = Some(value),
            "arg" => request.args.push(value),
            _ => return Err(invalid(format!("unknown field: {key}"))),
        }
    }
}

/// One frame of the server's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Out(Vec<u8>),
    Err(Vec<u8>),
    Exit(i32),
}

pub fn write_frame(out: &mut impl Write, frame: &Frame) -> io::Result<()> {
    match frame {
        Frame::Out(bytes) | Frame::Err(bytes) => {
            let kind = if matches!(frame, Frame::Out(_)) {
                "out"
            } else {
                "err"
            };
            out.write_all(format!("{kind} {}\n", bytes.len()).as_bytes())?;
            out.write_all(bytes)?;
        }
        Frame::Exit(code) => out.write_all(format!("exit {code}\n").as_bytes())?,
    }
    out.flush()
}

/// The next frame, or `None` when the server closed the connection.
pub fn read_frame(input: &mut impl BufRead) -> io::Result<Option<Frame>> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let (kind, number) = line
        .trim_end()
        .split_once(' ')
        .ok_or_else(|| invalid(format!("bad frame: {line:?}")))?;
    let number: i64 = number
        .parse()
        .map_err(|_| invalid(format!("bad frame: {line:?}")))?;
    match kind {
        "exit" => Ok(Some(Frame::Exit(number as i32))),
        "out" | "err" => {
            let mut bytes = vec![0; usize::try_from(number).map_err(|_| invalid("bad length"))?];
            input.read_exact(&mut bytes)?;
            Ok(Some(if kind == "out" {
                Frame::Out(bytes)
            } else {
                Frame::Err(bytes)
            }))
        }
        _ => Err(invalid(format!("bad frame: {line:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_survive_any_argument() {
        let request = Request {
            cwd: "/work/app\twith tab".into(),
            display: Some(":1".into()),
            args: vec![
                ":app:test".into(),
                "--tests".into(),
                "a%b\nc".into(),
                String::new(),
            ],
        };
        let mut wire = Vec::new();
        write_request(&mut wire, &request).unwrap();
        assert_eq!(read_request(&mut wire.as_slice()).unwrap(), request);
        assert!(read_request(&mut "hello\n".as_bytes()).is_err());
        assert!(
            read_request(&mut "gradle-run 1\ncwd\t/x\n".as_bytes()).is_err(),
            "no run line"
        );
    }

    #[test]
    fn frames_carry_bytes_and_the_exit_status() {
        let mut wire = Vec::new();
        for frame in [
            Frame::Out(b"line\n".to_vec()),
            Frame::Err(vec![0, 255, b'\n']),
            Frame::Exit(-1),
        ] {
            write_frame(&mut wire, &frame).unwrap();
        }
        let mut input = wire.as_slice();
        assert_eq!(
            read_frame(&mut input).unwrap(),
            Some(Frame::Out(b"line\n".to_vec()))
        );
        assert_eq!(
            read_frame(&mut input).unwrap(),
            Some(Frame::Err(vec![0, 255, b'\n']))
        );
        assert_eq!(read_frame(&mut input).unwrap(), Some(Frame::Exit(-1)));
        assert_eq!(read_frame(&mut input).unwrap(), None);
    }
}
