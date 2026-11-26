//! Approved GitHub repositories, for the relay. The relay tells pods apart, not agents, so a
//! repository is approved for a project's pod: until the pod is removed, for the project in
//! the policy, or for every project.

use crate::{ApprovalError, Style, child, edit_policy, error, io, parse, project_key};
use policy::Policy;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, TableLike, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoScope {
    /// Until the project's pod is removed.
    Session,
    /// `projects."<checkout>".network.github.repos`.
    Project,
    /// `network.github.repos`: every project.
    Global,
}

/// `owner/repo` as the user typed it.
pub fn parse_repository(value: &str) -> Result<String, ApprovalError> {
    if policy::is_repository(value) {
        Ok(value.to_owned())
    } else {
        Err(error(format!(
            "\"{value}\" must be a GitHub repository, owner/repo"
        )))
    }
}

/// GitHub's names are case-insensitive, and so is the relay's map.
fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// A project pod's repositories until the pod is removed: one per line, next to its agents'
/// session approvals, so removing the pod's approvals removes them too.
pub struct SessionRepos {
    file: PathBuf,
}

impl SessionRepos {
    pub fn new(state_dir: &Path, project_id: &str) -> SessionRepos {
        // Agent names never start with a dot, so this can't be an agent's file.
        SessionRepos {
            file: state_dir
                .join("approvals")
                .join(project_id)
                .join(".github-repos"),
        }
    }

    pub fn list(&self) -> Result<Vec<String>, ApprovalError> {
        let text = match fs::read_to_string(&self.file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(&self.file, e)),
        };
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| {
                parse_repository(line).map_err(|e| error(format!("{}: {e}", self.file.display())))
            })
            .collect()
    }

    /// Adds `repo`; false if it was already approved.
    pub fn add(&self, repo: &str) -> Result<bool, ApprovalError> {
        let mut list = self.list()?;
        if list.iter().any(|r| same(r, repo)) {
            return Ok(false);
        }
        list.push(repo.to_owned());
        self.write(&list)?;
        Ok(true)
    }

    /// Removes `repo`; false if it wasn't approved.
    pub fn remove(&self, repo: &str) -> Result<bool, ApprovalError> {
        let mut list = self.list()?;
        let before = list.len();
        list.retain(|r| !same(r, repo));
        if list.len() == before {
            return Ok(false);
        }
        self.write(&list)?;
        Ok(true)
    }

    fn write(&self, list: &[String]) -> Result<(), ApprovalError> {
        let folder = self.file.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(folder).map_err(|e| io(folder, e))?;
        let text: String = list.iter().map(|r| r.clone() + "\n").collect();
        fs::write(&self.file, text).map_err(|e| io(&self.file, e))
    }
}

/// Which `repos` list of the policy an edit changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoList<'a> {
    /// `[network.github] repos`.
    Global,
    /// `projects."<checkout>".network.github.repos`.
    Project { checkout: &'a Path, home: &'a Path },
}

/// Adds `repo` to `list` in the policy text; `None` if it's already there.
pub fn policy_add_repo(
    text: &str,
    list: RepoList,
    repo: &str,
) -> Result<Option<String>, ApprovalError> {
    let mut doc = parse(text)?;
    let repos = repos_array(&mut doc, list, true)?.expect("created");
    if position(repos, repo)?.is_some() {
        return Ok(None);
    }
    repos.push(repo);
    Ok(Some(doc.to_string()))
}

/// Removes `repo` from `list` in the policy text; `None` if it isn't there.
pub fn policy_remove_repo(
    text: &str,
    list: RepoList,
    repo: &str,
) -> Result<Option<String>, ApprovalError> {
    let mut doc = parse(text)?;
    let Some(repos) = repos_array(&mut doc, list, false)? else {
        return Ok(None);
    };
    let Some(i) = position(repos, repo)? else {
        return Ok(None);
    };
    repos.remove(i);
    Ok(Some(doc.to_string()))
}

fn position(repos: &Array, repo: &str) -> Result<Option<usize>, ApprovalError> {
    for (i, value) in repos.iter().enumerate() {
        let text = value
            .as_str()
            .ok_or_else(|| error(format!("repos lists hold strings, found {value}")))?;
        if same(text, repo) {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

/// The `repos` array of `list`, created on the way if `create`; layouts other than tables,
/// inline tables, dotted keys and arrays are refused, as for destinations.
fn repos_array<'d>(
    doc: &'d mut DocumentMut,
    list: RepoList,
    create: bool,
) -> Result<Option<&'d mut Array>, ApprovalError> {
    let root = doc.as_table_mut() as &mut dyn TableLike;
    let (table, path, outer, inner): (&mut dyn TableLike, String, Style, Style) = match list {
        // `[network.github]`, as the configuration guide writes it.
        RepoList::Global => (root, String::new(), Style::Implicit, Style::Header),
        RepoList::Project { checkout, home } => {
            let Some(projects) = child(root, "projects", create, Style::Implicit, "projects")?
            else {
                return Ok(None);
            };
            let key = project_key(projects, checkout, home)?;
            let name = format!("projects.\"{key}\".");
            let Some(project) = child(projects, &key, create, Style::Header, &name)? else {
                return Ok(None);
            };
            (project, name, Style::Dotted, Style::Dotted)
        }
    };
    let Some(network) = child(table, "network", create, outer, &format!("{path}network"))? else {
        return Ok(None);
    };
    let name = format!("{path}network.github");
    let Some(github) = child(network, "github", create, inner, &name)? else {
        return Ok(None);
    };
    if github.get("repos").is_none() {
        if !create {
            return Ok(None);
        }
        github.insert("repos", Item::Value(Value::Array(Array::new())));
    }
    match github.get_mut("repos").and_then(Item::as_array_mut) {
        Some(array) => Ok(Some(array)),
        None => Err(error(format!(
            "{name}.repos isn't an array; edit the policy by hand"
        ))),
    }
}

/// The project a repository approval is for.
pub struct RepoProject<'a> {
    pub checkout: &'a Path,
    pub id: &'a str,
    pub policy_file: &'a Path,
    pub home: &'a Path,
    pub state_dir: &'a Path,
}

fn listed(list: &[String], repo: &str) -> bool {
    list.iter().any(|r| same(r, repo))
}

/// Approves `repo` for the project's pod (or every project's); what to tell the user.
pub fn allow_repo(
    project: &RepoProject,
    policy: &Policy,
    repo: &str,
    scope: RepoScope,
    pod_running: bool,
) -> Result<Vec<String>, ApprovalError> {
    let checkout = project.checkout.display();
    let own = policy
        .projects
        .get(project.checkout)
        .map(|p| p.github_repos.as_slice())
        .unwrap_or_default();
    if listed(&policy.github_repos, repo) {
        return Ok(vec![format!(
            "{repo} is already readable by every project (network.github.repos)"
        )]);
    }
    if scope != RepoScope::Global && listed(own, repo) {
        return Ok(vec![format!(
            "{repo} is already readable in {checkout} by the policy"
        )]);
    }
    let (changed, lasts) = match scope {
        RepoScope::Session if !pod_running => {
            return Err(error(format!(
                "no session runs in {checkout}; a temporary approval lasts until its pod is removed. Use --project to keep it in the policy"
            )));
        }
        RepoScope::Session => (
            SessionRepos::new(project.state_dir, project.id).add(repo)?,
            format!("in {checkout} until the pod is removed"),
        ),
        RepoScope::Project => {
            let list = RepoList::Project {
                checkout: project.checkout,
                home: project.home,
            };
            let changed = edit_policy(project.policy_file, project.home, |text| {
                policy_add_repo(text, list, repo)
            })?;
            (changed, format!("in {checkout}, in the policy"))
        }
        RepoScope::Global => (
            edit_policy(project.policy_file, project.home, |text| {
                policy_add_repo(text, RepoList::Global, repo)
            })?,
            "in every project, in the policy".to_owned(),
        ),
    };
    if !changed {
        return Ok(vec![format!("{repo} is already approved {lasts}")]);
    }
    Ok(vec![format!(
        "grants read-only GitHub access to {repo} {lasts}"
    )])
}

/// Withdraws `repo` from the project (temporary and policy), or with `global` from the list
/// for every project; what to tell the user.
pub fn revoke_repo(
    project: &RepoProject,
    policy: &Policy,
    repo: &str,
    global: bool,
) -> Result<Vec<String>, ApprovalError> {
    let checkout = project.checkout.display();
    if global {
        let removed = edit_policy(project.policy_file, project.home, |text| {
            policy_remove_repo(text, RepoList::Global, repo)
        })?;
        if !removed {
            return Err(error(format!(
                "{repo} isn't on the global list (network.github.repos)"
            )));
        }
        let mut messages = vec![format!("removes {repo} from every project")];
        let keep: Vec<String> = policy
            .projects
            .iter()
            .filter(|(_, p)| listed(&p.github_repos, repo))
            .map(|(path, _)| path.display().to_string())
            .collect();
        if !keep.is_empty() {
            messages.push(format!(
                "still readable by their own tables: {}",
                keep.join(", ")
            ));
        }
        return Ok(messages);
    }
    let from_session = SessionRepos::new(project.state_dir, project.id).remove(repo)?;
    let list = RepoList::Project {
        checkout: project.checkout,
        home: project.home,
    };
    let from_policy = edit_policy(project.policy_file, project.home, |text| {
        policy_remove_repo(text, list, repo)
    })?;
    let everywhere = listed(&policy.github_repos, repo);
    if !from_session && !from_policy {
        return Err(error(if everywhere {
            format!(
                "{repo} is on the global list, for every project; `agent revoke --github {repo} --global` removes it there"
            )
        } else {
            format!("{repo} isn't approved in {checkout}")
        }));
    }
    let mut messages = vec![format!("revoked {repo} in {checkout}")];
    if everywhere {
        messages.push(
            "note: still readable through the global list; `--global` removes it there".into(),
        );
    }
    Ok(messages)
}
