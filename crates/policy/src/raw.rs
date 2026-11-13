//! The file's shape, as written. Unknown keys are errors everywhere.

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub version: u32,
    pub network: Option<Network>,
    pub mirror: Option<Mirror>,
    pub workspace: Option<Workspace>,
    pub agents: Option<Agents>,
    pub builds: Option<Builds>,
    #[serde(default)]
    pub projects: BTreeMap<String, Project>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Network {
    pub project: Option<Allow>,
    pub github: Option<Github>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allow {
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Github {
    #[serde(default)]
    pub repos: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mirror {
    #[serde(default)]
    pub remotes: BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    #[serde(default)]
    pub mask: Vec<String>,
    #[serde(default, rename = "read-only")]
    pub read_only: Vec<String>,
    #[serde(default)]
    pub private: Vec<String>,
}

#[derive(Deserialize, Default)]
pub struct Agents {
    pub memory: Option<String>,
    pub cpus: Option<f64>,
    /// `[agents.<name>]`: each agent's own destinations. Names are checked by the parser.
    #[serde(flatten)]
    pub named: BTreeMap<String, AgentNetwork>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentNetwork {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub local: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Builds {
    #[serde(rename = "max-concurrent")]
    pub max_concurrent: Option<u32>,
    pub memory: Option<String>,
    pub cpus: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub network: Option<ProjectNetwork>,
    pub builds: Option<ProjectBuilds>,
    pub engram: Option<String>,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentNetwork>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectNetwork {
    pub project: Option<Allow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBuilds {
    pub memory: Option<String>,
}
