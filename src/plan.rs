//! Squads and plans. A boss groups peers into squads and runs plans: steps that
//! run on a peer or on any member of a squad, in dependency order and in parallel
//! where possible. Each step gets the results of the steps it depends on, and steps
//! whose prerequisites failed are skipped.
//!
//! Plan text, one step per line (`//` starts a comment):
//!
//! ```text
//! api @alice:repo Summarize the public API of src/agent.rs
//! tests after api #qa:code using claude Write test ideas for this API
//! report after api,tests @helper Write a one-paragraph status report
//! ```
//!
//! `@peer` runs on that peer and `#squad` on the least busy online member of the
//! squad; `:folder` picks a folder the peer shares with you (default: its data
//! folder) and `using <provider>` a provider (default: copilot).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::agent::{Agent, PeerInfo};
use crate::llm::{LlmTask, Provider};
use crate::profile::validate_name;

/// Longest prerequisite result passed on to a dependent step.
const MAX_DEP_CHARS: usize = 8_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Squad {
    pub name: String,
    /// Allowlist names of the peers in the squad.
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SquadStore {
    #[serde(default)]
    pub squads: Vec<Squad>,
}

impl SquadStore {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    /// Creates or replaces squad `name`.
    pub fn set(&mut self, name: &str, members: Vec<String>) -> Result<()> {
        validate_name(name).context("invalid squad name")?;
        if members.is_empty() {
            bail!("a squad needs at least one member");
        }
        for member in &members {
            validate_name(member).with_context(|| format!("invalid member name {member:?}"))?;
        }
        let mut members = members;
        members.sort();
        members.dedup();
        self.squads.retain(|s| s.name != name);
        self.squads.push(Squad {
            name: name.to_owned(),
            members,
        });
        self.squads.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.squads.len();
        self.squads.retain(|s| s.name != name);
        self.squads.len() != before
    }

    pub fn get(&self, name: &str) -> Option<&Squad> {
        self.squads.iter().find(|s| s.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum Target {
    Peer(String),
    Squad(String),
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Peer(name) => write!(f, "@{name}"),
            Target::Squad(name) => write!(f, "#{name}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub after: Vec<String>,
    pub target: Target,
    pub share: Option<String>,
    pub provider: Provider,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub steps: Vec<Step>,
}

impl Plan {
    /// Parses and validates plan text (see the module docs). Reports every problem
    /// with its line number.
    pub fn parse(text: &str) -> Result<Self> {
        let mut steps = Vec::new();
        let mut errors = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            match parse_step(line) {
                Ok(step) => steps.push(step),
                Err(e) => errors.push(format!("line {}: {e}", n + 1)),
            }
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("\n"));
        }
        let plan = Plan { steps };
        plan.validate()?;
        Ok(plan)
    }

    /// Unique ids, known dependencies and no cycles.
    pub fn validate(&self) -> Result<()> {
        if self.steps.is_empty() {
            bail!("the plan has no steps");
        }
        let mut seen = HashMap::new();
        for step in &self.steps {
            if seen.insert(step.id.as_str(), step).is_some() {
                bail!("step '{}' is defined twice", step.id);
            }
        }
        for step in &self.steps {
            for dep in &step.after {
                if dep == &step.id {
                    bail!("step '{}' can't depend on itself", step.id);
                }
                if !seen.contains_key(dep.as_str()) {
                    bail!("step '{}' depends on unknown step '{dep}'", step.id);
                }
            }
        }
        // Kahn's algorithm: whatever can't be ordered is on a cycle.
        let mut pending: BTreeMap<&str, usize> = self
            .steps
            .iter()
            .map(|s| (s.id.as_str(), s.after.len()))
            .collect();
        let mut ready: Vec<&str> = pending
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(id, _)| *id)
            .collect();
        while let Some(done) = ready.pop() {
            pending.remove(done);
            for step in &self.steps {
                if step.after.iter().any(|d| d == done)
                    && let Some(n) = pending.get_mut(step.id.as_str())
                {
                    *n -= 1;
                    if *n == 0 {
                        ready.push(step.id.as_str());
                    }
                }
            }
        }
        if !pending.is_empty() {
            let stuck: Vec<&str> = pending.keys().copied().collect();
            bail!("dependency cycle between steps: {}", stuck.join(", "));
        }
        Ok(())
    }
}

fn parse_step(line: &str) -> Result<Step> {
    let mut words = line.split_whitespace().peekable();
    let id = words.next().context("missing step id")?.to_owned();
    validate_name(&id).map_err(|_| anyhow!("invalid step id {id:?}"))?;
    let mut after = Vec::new();
    if words.peek() == Some(&"after") {
        words.next();
        let deps = words
            .next()
            .context("'after' needs step ids, e.g. after a,b")?;
        after = deps
            .split(',')
            .filter(|d| !d.is_empty())
            .map(str::to_owned)
            .collect();
    }
    let target = words.next().context("missing target: @peer or #squad")?;
    let (kind, rest) = target.split_at(1);
    let (name, share) = match rest.split_once(':') {
        Some((name, share)) => (name, Some(share.to_owned()).filter(|s| !s.is_empty())),
        None => (rest, None),
    };
    validate_name(name).map_err(|_| anyhow!("invalid target {target:?}"))?;
    let target = match kind {
        "@" => Target::Peer(name.to_owned()),
        "#" => Target::Squad(name.to_owned()),
        _ => bail!("target must be @peer or #squad, got {target:?}"),
    };
    let mut provider = Provider::Copilot;
    if words.peek() == Some(&"using") {
        words.next();
        let name = words.next().context("'using' needs a provider")?;
        provider = clap::ValueEnum::from_str(name, true)
            .map_err(|_| anyhow!("unknown provider {name:?}"))?;
    }
    let prompt = words.collect::<Vec<_>>().join(" ");
    if prompt.is_empty() {
        bail!("missing the task prompt");
    }
    Ok(Step {
        id,
        after,
        target,
        share,
        provider,
        prompt,
    })
}

/// Where a step is in its life.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StepState {
    Waiting,
    Running { peer: String },
    Done { peer: String, output: String },
    Failed { peer: Option<String>, error: String },
    Skipped { reason: String },
}

impl StepState {
    fn finished(&self) -> bool {
        !matches!(self, StepState::Waiting | StepState::Running { .. })
    }
}

/// Picks the peer for `step`: the named peer, or the least busy online squad member
/// that serves the provider (and the folder, if one is named). Ties go by name.
pub fn choose_peer(
    step: &Step,
    peers: &[PeerInfo],
    squads: &SquadStore,
    busy: &HashMap<String, usize>,
) -> Result<PeerInfo> {
    let capable = |p: &&PeerInfo| {
        p.capabilities.contains(&step.provider)
            && step
                .share
                .as_ref()
                .is_none_or(|s| p.shares.iter().any(|shared| &shared.name == s))
    };
    match &step.target {
        Target::Peer(name) => {
            let peer = peers
                .iter()
                .find(|p| &p.name == name)
                .ok_or_else(|| anyhow!("{name} is not connected"))?;
            if !capable(&peer) {
                bail!(
                    "{name} doesn't serve {}{}",
                    step.provider,
                    step.share
                        .as_ref()
                        .map(|s| format!(" in a folder named '{s}' shared with you"))
                        .unwrap_or_default()
                );
            }
            Ok(peer.clone())
        }
        Target::Squad(name) => {
            let squad = squads
                .get(name)
                .ok_or_else(|| anyhow!("there is no squad named {name}"))?;
            peers
                .iter()
                .filter(|p| squad.members.contains(&p.name))
                .filter(capable)
                .min_by_key(|p| (busy.get(&p.name).copied().unwrap_or(0), p.name.clone()))
                .cloned()
                .ok_or_else(|| {
                    anyhow!(
                        "no online member of squad {name} serves {}{}",
                        step.provider,
                        step.share
                            .as_ref()
                            .map(|s| format!(" with a folder named '{s}'"))
                            .unwrap_or_default()
                    )
                })
        }
    }
}

/// The prompt a step runs with: the results of its prerequisites, then its task.
pub fn step_prompt(step: &Step, deps: &[(&str, &str, &str)]) -> String {
    if deps.is_empty() {
        return step.prompt.clone();
    }
    let mut prompt = String::from("Results of the earlier steps this task builds on:\n");
    for (id, peer, output) in deps {
        let mut text: String = output.chars().take(MAX_DEP_CHARS).collect();
        if output.chars().count() > MAX_DEP_CHARS {
            text += "\n[... truncated]";
        }
        prompt += &format!("\n### Step {id} (done by {peer})\n{text}\n");
    }
    prompt += &format!("\n---\nYour task:\n{}", step.prompt);
    prompt
}

/// Runs `plan` with `agent`, reporting every state change to `on_update`, and
/// returns each step's final state. Dropping the future cancels running steps.
pub async fn run_plan(
    agent: &Agent,
    squads: &SquadStore,
    plan: &Plan,
    mut on_update: impl FnMut(&str, &StepState),
) -> BTreeMap<String, StepState> {
    let mut states: BTreeMap<String, StepState> = plan
        .steps
        .iter()
        .map(|s| (s.id.clone(), StepState::Waiting))
        .collect();
    for step in &plan.steps {
        on_update(&step.id, &StepState::Waiting);
    }
    let mut busy: HashMap<String, usize> = HashMap::new();
    let mut running: JoinSet<(String, String, Result<String>)> = JoinSet::new();
    loop {
        // Skip steps whose prerequisites failed; start steps whose prerequisites are done.
        let mut changed = true;
        while changed {
            changed = false;
            for step in &plan.steps {
                if states[&step.id] != StepState::Waiting {
                    continue;
                }
                if let Some(dep) = step.after.iter().find(|d| {
                    matches!(
                        states[*d],
                        StepState::Failed { .. } | StepState::Skipped { .. }
                    )
                }) {
                    let state = StepState::Skipped {
                        reason: format!("step {dep} did not succeed"),
                    };
                    on_update(&step.id, &state);
                    states.insert(step.id.clone(), state);
                    changed = true;
                    continue;
                }
                if !step
                    .after
                    .iter()
                    .all(|d| matches!(states[d], StepState::Done { .. }))
                {
                    continue;
                }
                let peer = match choose_peer(step, &agent.peers(), squads, &busy) {
                    Ok(peer) => peer,
                    Err(e) => {
                        let state = StepState::Failed {
                            peer: None,
                            error: format!("{e:#}"),
                        };
                        on_update(&step.id, &state);
                        states.insert(step.id.clone(), state);
                        changed = true;
                        continue;
                    }
                };
                let deps: Vec<(&str, &str, &str)> = step
                    .after
                    .iter()
                    .filter_map(|d| match &states[d] {
                        StepState::Done { peer, output } => {
                            Some((d.as_str(), peer.as_str(), output.as_str()))
                        }
                        _ => None,
                    })
                    .collect();
                let mut task = LlmTask::new(step.provider, step_prompt(step, &deps));
                if let Some(share) = &step.share {
                    task = task.in_share(share.clone());
                }
                *busy.entry(peer.name.clone()).or_default() += 1;
                let state = StepState::Running {
                    peer: peer.name.clone(),
                };
                on_update(&step.id, &state);
                states.insert(step.id.clone(), state);
                let (agent, id) = (agent.clone(), step.id.clone());
                running.spawn(async move {
                    let result = agent.delegate(peer.id, task).await;
                    (id, peer.name, result)
                });
            }
        }
        let Some(joined) = running.join_next().await else {
            break;
        };
        let Ok((id, peer, result)) = joined else {
            continue;
        };
        if let Some(n) = busy.get_mut(&peer) {
            *n = n.saturating_sub(1);
        }
        let state = match result {
            Ok(output) => StepState::Done { peer, output },
            Err(e) => StepState::Failed {
                peer: Some(peer),
                error: format!("{e:#}"),
            },
        };
        on_update(&id, &state);
        states.insert(id, state);
    }
    debug_assert!(states.values().all(StepState::finished));
    states
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Access;
    use crate::shares::ShareInfo;

    #[test]
    fn parses_plan_text() {
        let plan = Plan::parse(
            "// release notes\n\
             api @alice:repo Summarize the public API\n\
             \n\
             tests after api #qa:code using claude Write test ideas for it\n\
             report after api,tests @helper Combine both into a status report\n",
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 3);
        let tests = &plan.steps[1];
        assert_eq!(tests.id, "tests");
        assert_eq!(tests.after, ["api"]);
        assert_eq!(tests.target, Target::Squad("qa".into()));
        assert_eq!(tests.share.as_deref(), Some("code"));
        assert_eq!(tests.provider, Provider::Claude);
        assert_eq!(tests.prompt, "Write test ideas for it");
        assert_eq!(plan.steps[0].target, Target::Peer("alice".into()));
        assert_eq!(plan.steps[0].provider, Provider::Copilot);
        assert_eq!(plan.steps[2].after, ["api", "tests"]);
        assert_eq!(plan.steps[2].share, None);

        let err = Plan::parse("a @x\nb nobody do it\nc @y using gpt hi").unwrap_err();
        let err = err.to_string();
        assert!(err.contains("line 1: missing the task prompt"), "{err}");
        assert!(
            err.contains("line 2: target must be @peer or #squad"),
            "{err}"
        );
        assert!(err.contains("line 3: unknown provider"), "{err}");
        assert!(Plan::parse("// only a comment").is_err());
    }

    #[test]
    fn rejects_cycles_and_unknown_dependencies() {
        let cycle = Plan::parse("a after c @x do\nb after a @x do\nc after b @x do\nd @x ok");
        assert!(
            cycle
                .unwrap_err()
                .to_string()
                .contains("cycle between steps: a, b, c")
        );
        let unknown = Plan::parse("a after z @x do").unwrap_err().to_string();
        assert!(unknown.contains("unknown step 'z'"), "{unknown}");
        let own = Plan::parse("a after a @x do").unwrap_err().to_string();
        assert!(own.contains("itself"), "{own}");
        let twice = Plan::parse("a @x do\na @y again").unwrap_err().to_string();
        assert!(twice.contains("defined twice"), "{twice}");
    }

    #[test]
    fn squads_persist_and_validate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("squads.json");
        let mut store = SquadStore::default();
        store
            .set("qa", vec!["bob".into(), "alice".into(), "bob".into()])
            .unwrap();
        assert_eq!(store.get("qa").unwrap().members, ["alice", "bob"]);
        assert!(store.set("qa", vec![]).is_err());
        assert!(store.set("../x", vec!["a".into()]).is_err());
        assert!(store.set("dev", vec!["bad name".into()]).is_err());
        store.save(&path).unwrap();
        let loaded = SquadStore::load(&path).unwrap();
        assert_eq!(loaded, store);
        let mut loaded = loaded;
        assert!(loaded.remove("qa"));
        assert!(!loaded.remove("qa"));
        assert_eq!(
            SquadStore::load(&dir.path().join("none")).unwrap(),
            SquadStore::default()
        );
    }

    #[test]
    fn squad_steps_pick_the_least_busy_capable_member() {
        let peer = |name: &str, providers: Vec<Provider>, shares: &[&str]| PeerInfo {
            name: name.into(),
            id: iroh::SecretKey::generate().public(),
            capabilities: providers,
            shares: shares
                .iter()
                .map(|s| ShareInfo {
                    name: (*s).into(),
                    access: Access::ReadOnly,
                })
                .collect(),
            owner: None,
            role: None,
        };
        let peers = vec![
            peer("alice", vec![Provider::Copilot], &["data", "code"]),
            peer("bob", vec![Provider::Copilot], &["data", "code"]),
            peer("carol", vec![Provider::Claude], &["data", "code"]),
            peer("dave", vec![Provider::Copilot], &["data"]),
            peer("outsider", vec![Provider::Copilot], &["data", "code"]),
        ];
        let mut squads = SquadStore::default();
        squads
            .set(
                "qa",
                vec![
                    "alice".into(),
                    "bob".into(),
                    "carol".into(),
                    "dave".into(),
                    "erin".into(),
                ],
            )
            .unwrap();
        let step = Plan::parse("t #qa:code test it").unwrap().steps.remove(0);

        let mut busy = HashMap::new();
        assert_eq!(
            choose_peer(&step, &peers, &squads, &busy).unwrap().name,
            "alice"
        );
        busy.insert("alice".to_owned(), 1);
        assert_eq!(
            choose_peer(&step, &peers, &squads, &busy).unwrap().name,
            "bob"
        );
        busy.insert("bob".to_owned(), 2);
        assert_eq!(
            choose_peer(&step, &peers, &squads, &busy).unwrap().name,
            "alice",
            "carol lacks copilot, dave lacks the folder, erin is offline, outsider isn't a member"
        );

        let claude = Plan::parse("t #qa using claude review")
            .unwrap()
            .steps
            .remove(0);
        assert_eq!(
            choose_peer(&claude, &peers, &squads, &busy).unwrap().name,
            "carol"
        );
        let codex = Plan::parse("t #qa using codex review")
            .unwrap()
            .steps
            .remove(0);
        assert!(choose_peer(&codex, &peers, &squads, &busy).is_err());
        let nosquad = Plan::parse("t #ops review").unwrap().steps.remove(0);
        assert!(choose_peer(&nosquad, &peers, &squads, &busy).is_err());
        let direct = Plan::parse("t @dave:code review").unwrap().steps.remove(0);
        assert!(choose_peer(&direct, &peers, &squads, &busy).is_err());
    }
}
