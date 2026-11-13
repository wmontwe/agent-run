//! Approved egress destinations: session
//! approvals, kept in the state folder until the project's pod is removed, and project and
//! global approvals, edited in `policy.toml` with the user's comments and layout kept.
//!
//! A session approval is a file because `agent allow` runs apart from the sessions, and every
//! start and end rebuilds the proxy's rules from the policy and these files.

mod commands;

pub use commands::{Project, Scope, allow, parse_destination, revoke};

use policy::{Destination, destination, project_path};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalError(pub String);

impl fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ApprovalError {}

fn error(message: impl Into<String>) -> ApprovalError {
    ApprovalError(message.into())
}

fn io(path: &Path, e: std::io::Error) -> ApprovalError {
    error(format!("{}: {e}", path.display()))
}

/// How a destination is written: `host`, or `host:port` for other ports than 443.
pub fn written(destination: &Destination) -> String {
    destination.short()
}

/// One agent's session approvals in a project: one destination per line in the state folder.
pub struct Session {
    file: PathBuf,
}

impl Session {
    pub fn new(state_dir: &Path, project_id: &str, agent: &str) -> Session {
        Session {
            file: state_dir.join("approvals").join(project_id).join(agent),
        }
    }

    /// Forgets every agent's session approvals in the project; with the pod.
    pub fn clear_project(state_dir: &Path, project_id: &str) -> Result<(), ApprovalError> {
        let dir = state_dir.join("approvals").join(project_id);
        match fs::remove_dir_all(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(&dir, e)),
            _ => Ok(()),
        }
    }

    pub fn list(&self) -> Result<Vec<Destination>, ApprovalError> {
        let text = match fs::read_to_string(&self.file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(&self.file, e)),
        };
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                destination(line.trim()).map_err(|e| error(format!("{}: {e}", self.file.display())))
            })
            .collect()
    }

    /// Adds `destination`; false if it was already approved.
    pub fn add(&self, destination: &Destination) -> Result<bool, ApprovalError> {
        let mut list = self.list()?;
        if list.contains(destination) {
            return Ok(false);
        }
        list.push(destination.clone());
        self.write(&list)?;
        Ok(true)
    }

    /// Removes `destination`; false if it wasn't approved.
    pub fn remove(&self, destination: &Destination) -> Result<bool, ApprovalError> {
        let mut list = self.list()?;
        let before = list.len();
        list.retain(|d| d != destination);
        if list.len() == before {
            return Ok(false);
        }
        self.write(&list)?;
        Ok(true)
    }

    fn write(&self, list: &[Destination]) -> Result<(), ApprovalError> {
        let folder = self.file.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(folder).map_err(|e| io(folder, e))?;
        let text: String = list.iter().map(|d| written(d) + "\n").collect();
        fs::write(&self.file, text).map_err(|e| io(&self.file, e))
    }
}

/// Which list of the policy an edit changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List<'a> {
    /// `network.project.allow`: every project, builds included.
    Global,
    /// `projects."<checkout>".agents.<agent>.allow`: one agent's approvals in one project.
    Agent {
        checkout: &'a Path,
        home: &'a Path,
        agent: &'a str,
    },
}

/// Adds `destination` to `list` in the policy text; `None` if it's already there. A project
/// without a table gets one, named with `~/` when the checkout is in `home`.
pub fn policy_add(
    text: &str,
    list: List,
    destination: &Destination,
) -> Result<Option<String>, ApprovalError> {
    let mut doc = parse(text)?;
    let allow = allow_array(&mut doc, list, true)?.expect("created");
    if contains(allow, destination)? {
        return Ok(None);
    }
    allow.push(written(destination));
    Ok(Some(doc.to_string()))
}

/// Removes `destination` from `list` in the policy text; `None` if it isn't there.
pub fn policy_remove(
    text: &str,
    list: List,
    destination: &Destination,
) -> Result<Option<String>, ApprovalError> {
    let mut doc = parse(text)?;
    let Some(allow) = allow_array(&mut doc, list, false)? else {
        return Ok(None);
    };
    let mut found = None;
    for (i, value) in allow.iter().enumerate() {
        if same(value, destination)? {
            found = Some(i);
        }
    }
    let Some(i) = found else { return Ok(None) };
    allow.remove(i);
    Ok(Some(doc.to_string()))
}

/// Whether the policy text's `list` contains `destination`.
pub fn policy_contains(
    text: &str,
    list: List,
    destination: &Destination,
) -> Result<bool, ApprovalError> {
    let mut doc = parse(text)?;
    match allow_array(&mut doc, list, false)? {
        Some(allow) => contains(allow, destination),
        None => Ok(false),
    }
}

/// Applies an edit to the policy file: reads it, edits, checks the result with the policy's
/// own strict parser, and writes it in place. `Ok(false)` if the edit changed nothing.
pub fn edit_policy(
    path: &Path,
    home: &Path,
    edit: impl FnOnce(&str) -> Result<Option<String>, ApprovalError>,
) -> Result<bool, ApprovalError> {
    let text = fs::read_to_string(path).map_err(|e| io(path, e))?;
    let Some(changed) = edit(&text)? else {
        return Ok(false);
    };
    policy::parse(&changed, home).map_err(|e| {
        error(format!(
            "{}: the edit would make the policy invalid, so it wasn't written: {e}",
            path.display()
        ))
    })?;
    fs::write(path, changed).map_err(|e| io(path, e))?;
    Ok(true)
}

fn parse(text: &str) -> Result<DocumentMut, ApprovalError> {
    text.parse::<DocumentMut>()
        .map_err(|e| error(format!("the policy isn't valid TOML: {e}")))
}

fn contains(allow: &Array, destination: &Destination) -> Result<bool, ApprovalError> {
    for value in allow.iter() {
        if same(value, destination)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn same(value: &Value, destination: &Destination) -> Result<bool, ApprovalError> {
    let text = value
        .as_str()
        .ok_or_else(|| error(format!("allow lists hold strings, found {value}")))?;
    Ok(policy::destination(text).map_err(|e| error(e.to_string()))? == *destination)
}

/// The `allow` array of `list`, created on the way if `create`. Layouts other than tables,
/// inline tables, dotted keys and arrays are refused, not guessed.
fn allow_array<'d>(
    doc: &'d mut DocumentMut,
    list: List,
    create: bool,
) -> Result<Option<&'d mut Array>, ApprovalError> {
    let root = doc.as_table_mut() as &mut dyn TableLike;
    let (table, path): (&mut dyn TableLike, String) = match list {
        List::Global => (root, String::new()),
        List::Agent { checkout, home, .. } => {
            let Some(projects) = child(root, "projects", create, Style::Implicit, "projects")?
            else {
                return Ok(None);
            };
            let key = project_key(projects, checkout, home)?;
            let name = format!("projects.\"{key}\".");
            let Some(project) = child(projects, &key, create, Style::Header, &name)? else {
                return Ok(None);
            };
            (project, name)
        }
    };
    // Inside a project table, dotted keys keep the path on one line, as in the design's sample;
    // the global list gets a `[network.project]` header.
    let (outer, inner, first, second) = match list {
        List::Global => (Style::Implicit, Style::Header, "network", "project"),
        List::Agent { agent, .. } => (Style::Dotted, Style::Dotted, "agents", agent),
    };
    let Some(network) = child(table, first, create, outer, &format!("{path}{first}"))? else {
        return Ok(None);
    };
    let Some(project) = child(
        network,
        second,
        create,
        inner,
        &format!("{path}{first}.{second}"),
    )?
    else {
        return Ok(None);
    };
    let key = format!("{path}{first}.{second}.allow");
    if project.get("allow").is_none() {
        if !create {
            return Ok(None);
        }
        project.insert("allow", Item::Value(Value::Array(Array::new())));
    }
    match project.get_mut("allow").and_then(Item::as_array_mut) {
        Some(array) => Ok(Some(array)),
        None => Err(error(format!(
            "{key} isn't an array; edit the policy by hand"
        ))),
    }
}

/// How a table created for an edit is written.
#[derive(Clone, Copy)]
enum Style {
    /// No header of its own: only its subtables have one.
    Implicit,
    /// `[name]`.
    Header,
    /// `name.key = value` inside its parent.
    Dotted,
}

fn child<'t>(
    table: &'t mut dyn TableLike,
    key: &str,
    create: bool,
    style: Style,
    name: &str,
) -> Result<Option<&'t mut dyn TableLike>, ApprovalError> {
    if table.get(key).is_none() {
        if !create {
            return Ok(None);
        }
        let mut new = Table::new();
        new.set_implicit(matches!(style, Style::Implicit | Style::Dotted));
        new.set_dotted(matches!(style, Style::Dotted));
        table.insert(key, Item::Table(new));
    }
    match table.get_mut(key).and_then(Item::as_table_like_mut) {
        Some(child) => Ok(Some(child)),
        None => Err(error(format!(
            "{name} isn't a table; edit the policy by hand"
        ))),
    }
}

/// The key of the project's table: the existing one naming `checkout`, or a new one.
fn project_key(
    projects: &dyn TableLike,
    checkout: &Path,
    home: &Path,
) -> Result<String, ApprovalError> {
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let target = canonical(checkout);
    for (key, _) in projects.iter() {
        let path = project_path(key, home).map_err(|e| error(e.to_string()))?;
        if canonical(&path) == target {
            return Ok(key.to_owned());
        }
    }
    Ok(match checkout.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => checkout.display().to_string(),
    })
}

#[cfg(test)]
mod tests;
