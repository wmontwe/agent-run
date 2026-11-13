//! Approving and revoking destinations against a real session,
//! including what happens to a tunnel that's open while its destination
//! is revoked. Needs the Podman machine and the internet.
//! Run with `cargo test -p agent --test approvals -- --ignored`.

mod common;

use agent::{approve, session};
use approvals::Scope;
use std::fs;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

/// `"<CONNECT status> <HTTP status>"` of an HTTPS request from the session's container.
fn fetch(container: &str, url: &str) -> String {
    let output = Command::new("podman")
        .args([
            "exec",
            container,
            "curl",
            "-s",
            "-o",
            "/dev/null",
            "--max-time",
            "8",
            "-w",
            "%{http_connect} %{http_code}",
            url,
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn exec(container: &str, script: &str) -> String {
    let output = Command::new("podman")
        .args(["exec", container, "sh", "-c", script])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A request over a tunnel opened before `/tmp/go` appears, and one more over the same
/// tunnel after: shows whether a revocation cuts open tunnels.
const TUNNEL: &str = r#"
const http = require("http"), tls = require("tls"), fs = require("fs");
const req = http.request({ host: "proxy", port: 3128, method: "CONNECT", path: "example.org:443" });
req.on("connect", (res, socket) => {
  if (res.statusCode !== 200) { console.log("connect " + res.statusCode); process.exit(1); }
  const t = tls.connect({ socket, servername: "example.org" }, () => {
    let n = 0;
    const get = () => t.write("GET / HTTP/1.1\r\nHost: example.org\r\n\r\n");
    t.on("data", (d) => {
      const s = d.toString();
      if (!s.startsWith("HTTP/1.1")) return;
      n++;
      console.log("response " + n + " " + s.split("\r\n")[0]);
      if (n === 1) { const wait = () => fs.existsSync("/tmp/go") ? get() : setTimeout(wait, 200); wait(); }
      else process.exit(0);
    });
    t.on("close", () => { console.log("closed"); process.exit(3); });
    get();
  });
});
req.end();
setTimeout(() => { console.log("timeout"); process.exit(2); }, 60000);
"#;

#[test]
#[ignore = "needs the Podman machine and the internet"]
fn approvals_apply_live_and_revocations_leave_open_tunnels() {
    let root = common::checkout("approvals");
    let scratch = common::Scratch::new("approvals");
    scratch.policy("version = 1\n\n[network.project]   # every project\nallow = [\"api.anthropic.com\", \"example.com\"]\n");
    let base = scratch.base.clone();
    let paths = || scratch.paths();
    let proxy = scratch.namespace.name("egress");
    let session = session::start(
        "claude",
        &root,
        Some(&["sleep".into(), "600".into()]),
        paths(),
    )
    .expect("session starts");
    let container = session.name.clone();
    let network = session.project.network();
    let mut failures = Vec::new();
    let mut expect = |name: &str, actual: String, expected: &str| {
        if actual != expected {
            failures.push(format!("{name}: {actual}, expected {expected}"));
        }
    };

    expect(
        "before approval",
        fetch(&container, "https://example.org"),
        "403 000",
    );
    approve::allow(
        &root.join("sub"),
        "example.org",
        Scope::Session,
        None,
        &paths(),
    )
    .expect("session approval");
    approve::allow(&root, "example.net", Scope::Project, None, &paths()).expect("project approval");
    sleep(Duration::from_secs(1));
    expect(
        "session approval",
        fetch(&container, "https://example.org"),
        "200 200",
    );
    expect(
        "project approval",
        fetch(&container, "https://example.net"),
        "200 200",
    );
    let policy = fs::read_to_string(base.join("policy.toml")).unwrap();
    let table = format!(
        "[projects.\"{}\"]\nagents.claude.allow = [\"example.net\"]",
        root.display()
    );
    expect(
        "project approval in the policy, comments kept",
        (policy.contains("# every project") && policy.contains(&table)).to_string(),
        "true",
    );

    // A tunnel opened before the revocation, used again after it.
    let mut child = Command::new("podman")
        .args(["exec", "-i", &container, "sh", "-c", "cat > /tmp/tunnel.js"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(TUNNEL.as_bytes())
        .unwrap();
    child.wait().unwrap();
    let tunnel = Command::new("podman")
        .args(["exec", &container, "node", "/tmp/tunnel.js"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    sleep(Duration::from_secs(3));
    // Another agent in the same pod reaches none of Claude's approvals.
    let pi = session::start(
        "pi",
        &root,
        Some(&["sleep".into(), "600".into()]),
        scratch.paths_for("pi"),
    )
    .expect("pi session");
    expect(
        "another agent's session approval",
        fetch(&pi.name, "https://example.org"),
        "403 000",
    );
    expect(
        "another agent's project approval",
        fetch(&pi.name, "https://example.net"),
        "403 000",
    );
    expect(
        "the shared list for every agent",
        fetch(&pi.name, "https://example.com"),
        "200 200",
    );
    let ambiguous =
        approve::allow(&root, "example.edu", Scope::Session, None, &paths()).unwrap_err();
    expect(
        "with two agents running, an approval names its agent",
        ambiguous.contains("--agent").to_string(),
        "true",
    );
    pi.end();

    // A revocation whose reload failed: gone from the store, still in the proxy's rules.
    let org = approvals::parse_destination("example.org").unwrap();
    assert!(
        approvals::Session::new(&paths().state, &session.project.id, "claude")
            .remove(&org)
            .unwrap()
    );
    let again = approve::revoke(&root, "example.org", false, None, &paths()).unwrap_err();
    expect(
        "repeating it says it's gone",
        again.contains("isn't approved").to_string(),
        "true",
    );
    sleep(Duration::from_secs(1));
    exec(&container, "touch /tmp/go");
    let tunnel = String::from_utf8_lossy(&tunnel.wait_with_output().unwrap().stdout)
        .trim()
        .to_owned();
    eprintln!("open tunnel across the revocation: {tunnel:?}");

    expect(
        "after revocation",
        fetch(&container, "https://example.org"),
        "403 000",
    );
    expect(
        "other approvals unaffected",
        fetch(&container, "https://example.net"),
        "200 200",
    );
    let global = approve::revoke(&root, "example.com", false, None, &paths()).unwrap_err();
    expect(
        "a global host needs --global",
        global.contains("--global").to_string(),
        "true",
    );
    approve::revoke(&root, "example.com", true, None, &paths()).expect("global revoke");
    sleep(Duration::from_secs(1));
    expect(
        "after a global revocation",
        fetch(&container, "https://example.com"),
        "403 000",
    );
    expect(
        "IP addresses aren't approvable",
        approve::allow(&root, "1.1.1.1", Scope::Session, None, &paths())
            .is_err()
            .to_string(),
        "true",
    );
    let log = Command::new("podman")
        .args(["logs", &proxy])
        .output()
        .unwrap();
    let subnet = egress::subnet(&podman::Podman::default(), &network).unwrap();
    let denials = egress::denials(&String::from_utf8_lossy(&log.stdout), &subnet, 20);
    expect(
        "denials list the revoked host",
        denials
            .iter()
            .any(|d| d.destination == "example.org:443")
            .to_string(),
        "true",
    );

    session.end();
    let approvals = base.join("state/approvals");
    expect(
        "session approvals gone with the pod",
        fs::read_dir(&approvals)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true)
            .to_string(),
        "true",
    );
    let _ = fs::remove_dir_all(&root);
    // As the design says: a revocation refuses new connections; open ones stay until they close.
    assert_eq!(
        tunnel, "response 1 HTTP/1.1 200 OK\nresponse 2 HTTP/1.1 200 OK",
        "the open tunnel across the revocation"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
