//! Squads and plans. A boss groups peers into squads and runs plans: steps that
//! run on a peer or on any member of a squad, in dependency order and in parallel
//! where possible. Each step gets the results of the steps it depends on, and steps
//! whose prerequisites failed are skipped.
//!
//! Plan text, one claim or step per line (`//` starts a comment):
//!
//! ```text
//! claim: C-1 The parser handles empty input
//! api @alice:repo Summarize the public API of src/agent.rs
//! fix after api @bob:app verify=test attempts=3 claims=C-1 Make the parser accept empty input
//! review after fix #qa:app apply=fix verify=test claims=C-1 Check the fix independently
//! signoff after review approve @carol claims=C-1 Approve the change for release
//! ```
//!
//! `@peer` runs on that peer and `#squad` on the least busy online member of the
//! squad; `:folder` picks a folder the peer shares with you (default: its data
//! folder) and `using <provider>` a provider (default: copilot). Options before the
//! prompt: `verify=a,b` asks the peer's runtime to run those owner-defined checks
//! after the model (as signed evidence), `attempts=N` retries with the failures,
//! `claims=C-1` says which claims the step proves and `apply=step` applies that
//! prerequisite's commit first. `approve @person` (or `@me`) asks a team member to
//! sign off instead of running a model. See [`crate::runs`] for the verdicts.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::agent::{Agent, Delivered, PeerInfo};
use crate::evidence::Evidence;
use crate::knowledge::{KnowledgeBase, check_topics};
use crate::llm::{LlmTask, Provider};
use crate::profile::validate_name;
use crate::skills::Skills;

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
    /// Owner-defined checks the peer's runtime runs after the model (`verify=a,b`).
    #[serde(default)]
    pub checks: Vec<String>,
    /// Model attempts while checks fail (`attempts=N`).
    #[serde(default)]
    pub attempts: Option<u32>,
    /// Claims this step provides evidence for (`claims=C-1,C-2`).
    #[serde(default)]
    pub claims: Vec<String>,
    /// A prerequisite whose commit (patch) is applied before this step (`apply=step`).
    #[serde(default)]
    pub apply: Option<String>,
    /// A team member's sign-off instead of an LLM task (`approve @person`).
    #[serde(default)]
    pub approve: bool,
    /// Knowledge base topics put in the prompt (`context=a,b` or `context=all`).
    #[serde(default)]
    pub context: Vec<String>,
    /// Knowledge base topic the step's verified result is saved as (`save=topic`).
    #[serde(default)]
    pub save: Option<String>,
    /// Skills (instructions) put in the prompt (`skill=a,b`).
    #[serde(default)]
    pub skills: Vec<String>,
}

/// A statement the plan must prove with evidence (`claim: ID text`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDef {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub claims: Vec<ClaimDef>,
    pub steps: Vec<Step>,
}

impl Plan {
    /// Parses and validates plan text (see the module docs). Reports every problem
    /// with its line number.
    pub fn parse(text: &str) -> Result<Self> {
        let mut steps = Vec::new();
        let mut claims = Vec::new();
        let mut errors = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let parsed = match line.strip_prefix("claim:") {
                Some(rest) => parse_claim(rest).map(|c| claims.push(c)),
                None => parse_step(line).map(|s| steps.push(s)),
            };
            if let Err(e) = parsed {
                errors.push(format!("line {}: {e}", n + 1));
            }
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("\n"));
        }
        let plan = Plan { claims, steps };
        plan.validate()?;
        Ok(plan)
    }

    /// Unique ids, known dependencies and claims, sensible options and no cycles.
    pub fn validate(&self) -> Result<()> {
        if self.steps.is_empty() {
            bail!("the plan has no steps");
        }
        let mut claim_ids = HashMap::new();
        for claim in &self.claims {
            if claim_ids.insert(claim.id.as_str(), claim).is_some() {
                bail!("claim '{}' is defined twice", claim.id);
            }
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
            if let Some(claim) = step
                .claims
                .iter()
                .find(|c| !claim_ids.contains_key(c.as_str()))
            {
                bail!(
                    "step '{}' proves unknown claim '{claim}'; define it first with a line: claim: {claim} <what must be true>",
                    step.id
                );
            }
            if let Some(apply) = &step.apply
                && !step.after.contains(apply)
            {
                bail!(
                    "step '{}' applies the work of '{apply}', so it must run after it (after {apply})",
                    step.id
                );
            }
            if step.attempts.is_some() && step.checks.is_empty() {
                bail!(
                    "step '{}' sets attempts but no checks to retry on (verify=...)",
                    step.id
                );
            }
            if step.approve
                && (!step.checks.is_empty() || step.apply.is_some() || step.share.is_some())
            {
                bail!(
                    "approval step '{}' can't have checks, a folder or a patch",
                    step.id
                );
            }
            if step.approve && step.save.is_some() {
                bail!("approval step '{}' has no result to save", step.id);
            }
            if step.approve && matches!(step.target, Target::Squad(_)) {
                bail!("approval step '{}' needs a person: @name or @me", step.id);
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

impl Plan {
    /// The plan as it is sent: each step's `skill=` instructions and `context=`
    /// knowledge put in front of its prompt. Fails if a skill or topic doesn't exist.
    pub fn prepare(&self, kb: &KnowledgeBase, skills: &Skills) -> Result<Plan> {
        let mut plan = self.with_knowledge(kb)?;
        for step in &mut plan.steps {
            if step.skills.is_empty() {
                continue;
            }
            let instructions = skills
                .instructions(&step.skills)
                .with_context(|| format!("step '{}'", step.id))?;
            step.prompt = format!(
                "Follow these instructions:\n{instructions}\n---\n{}",
                step.prompt
            );
        }
        Ok(plan)
    }

    /// The plan with each step's `context=` knowledge put in front of its prompt.
    /// Fails if a topic doesn't exist.
    pub fn with_knowledge(&self, kb: &KnowledgeBase) -> Result<Plan> {
        let mut plan = self.clone();
        for step in &mut plan.steps {
            if step.context.is_empty() {
                continue;
            }
            check_topics(kb, &step.context).with_context(|| format!("step '{}'", step.id))?;
            let knowledge = kb.context(&step.context)?;
            step.prompt = format!(
                "Knowledge about this system (from earlier, verified work):\n{knowledge}\n---\n{}",
                step.prompt
            );
        }
        Ok(plan)
    }
}

/// Saves the result of every finished step with `save=` into `kb`, if its evidence
/// was verified (or it ran locally); returns the topics written.
pub fn save_knowledge(
    kb: &KnowledgeBase,
    plan: &Plan,
    states: &BTreeMap<String, StepState>,
    run_id: &str,
) -> Vec<String> {
    let mut saved = Vec::new();
    for step in &plan.steps {
        let (
            Some(topic),
            Some(StepState::Done {
                peer,
                output,
                verified: true,
                ..
            }),
        ) = (&step.save, states.get(&step.id))
        else {
            continue;
        };
        let source = format!(
            "run {run_id}, step {} by {peer} (signed, verified)",
            step.id
        );
        if kb.write(topic, output, &source).is_ok() {
            saved.push(topic.clone());
        }
    }
    saved
}

fn parse_claim(rest: &str) -> Result<ClaimDef> {
    let rest = rest.trim();
    let (id, text) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    validate_name(id).map_err(|_| anyhow!("invalid claim id {id:?}"))?;
    let text = text.trim();
    if text.is_empty() {
        bail!("claim {id} needs a statement: claim: {id} <what must be true>");
    }
    Ok(ClaimDef {
        id: id.to_owned(),
        text: text.to_owned(),
    })
}

fn list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .collect()
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
    let approve = words.peek() == Some(&"approve");
    if approve {
        words.next();
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
    let (mut checks, mut attempts, mut claims, mut apply) = (Vec::new(), None, Vec::new(), None);
    let (mut context, mut save, mut skills) = (Vec::new(), None, Vec::new());
    loop {
        match words.peek().copied() {
            Some("using") => {
                words.next();
                let name = words.next().context("'using' needs a provider")?;
                provider = clap::ValueEnum::from_str(name, true)
                    .map_err(|_| anyhow!("unknown provider {name:?}"))?;
            }
            Some(word) if word.contains('=') => {
                let (key, value) = word.split_once('=').expect("contains '='");
                match key {
                    "verify" => checks = list(value),
                    "attempts" => {
                        let n: u32 = value
                            .parse()
                            .map_err(|_| anyhow!("attempts must be a number, got {value:?}"))?;
                        if !(1..=crate::evidence::MAX_ATTEMPTS).contains(&n) {
                            bail!(
                                "attempts must be between 1 and {}",
                                crate::evidence::MAX_ATTEMPTS
                            );
                        }
                        attempts = Some(n);
                    }
                    "claims" | "claim" => claims = list(value),
                    "apply" => apply = Some(value.to_owned()).filter(|v| !v.is_empty()),
                    "context" => context = list(value),
                    "skill" | "skills" => skills = list(value),
                    "save" => {
                        validate_name(value)
                            .map_err(|_| anyhow!("save= needs a topic name, got {value:?}"))?;
                        save = Some(value.to_owned());
                    }
                    _ => break,
                }
                words.next();
            }
            _ => break,
        }
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
        checks,
        attempts,
        claims,
        apply,
        approve,
        context,
        save,
        skills,
    })
}

/// Where a step is in its life.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StepState {
    Waiting,
    Running {
        peer: String,
    },
    Done {
        peer: String,
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<Evidence>,
        /// The evidence is signed by `peer` for exactly this output.
        #[serde(default)]
        verified: bool,
        /// The step's commit as a patch (kept out of JSON; saved next to run reports).
        #[serde(skip)]
        patch: Option<String>,
    },
    Failed {
        peer: Option<String>,
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<Evidence>,
        #[serde(default)]
        verified: bool,
    },
    Skipped {
        reason: String,
    },
}

impl StepState {
    pub fn finished(&self) -> bool {
        !matches!(self, StepState::Waiting | StepState::Running { .. })
    }

    pub fn failed(peer: Option<String>, error: String) -> Self {
        StepState::Failed {
            peer,
            error,
            evidence: None,
            verified: false,
        }
    }

    /// The step's state from what `peer` sent back. Evidence that isn't signed by
    /// `peer`, or missing evidence for requested checks, fails the step.
    fn from_delivered(peer: String, step: &Step, d: Delivered) -> Self {
        let problem = match &d.evidence {
            Some(_) if !d.verified => Some(format!("the evidence isn't validly signed by {peer}")),
            None if !step.checks.is_empty() || step.approve => {
                Some(format!("{peer} sent no evidence (an older version?)"))
            }
            _ => None,
        };
        if d.ok && problem.is_none() {
            return StepState::Done {
                peer,
                output: d.output,
                evidence: d.evidence,
                verified: d.verified,
                patch: d.patch,
            };
        }
        StepState::Failed {
            peer: Some(peer),
            error: match problem {
                Some(problem) => format!("{problem}\n\n{}", d.output),
                None => d.output,
            },
            evidence: d.evidence,
            verified: d.verified,
        }
    }
}

/// A problem `preflight` found: errors stop the plan from running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub step: Option<String>,
    pub error: bool,
    pub message: String,
}

/// Why `peer` can't run `step`, if it can't: provider, folder, checks, access.
fn unable(step: &Step, peer: &PeerInfo) -> Option<String> {
    use crate::llm::Access;
    if !peer.capabilities.contains(&step.provider) {
        return Some(format!("{} doesn't serve {}", peer.name, step.provider));
    }
    let folder = step.share.as_deref().unwrap_or(crate::shares::DATA_SHARE);
    let Some(share) = peer.shares.iter().find(|s| s.name == folder) else {
        let shared: Vec<&str> = peer.shares.iter().map(|s| s.name.as_str()).collect();
        return Some(format!(
            "{} shares no folder '{folder}' with you (it shares: {})",
            peer.name,
            if shared.is_empty() {
                "none".into()
            } else {
                shared.join(", ")
            }
        ));
    };
    let missing: Vec<&str> = step
        .checks
        .iter()
        .filter(|c| !share.checks.contains(c))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Some(format!(
            "folder '{folder}' on {} has no check {} (it has: {}); its owner adds checks with share add --check NAME=COMMAND",
            peer.name,
            missing.join(", "),
            if share.checks.is_empty() {
                "none".into()
            } else {
                share.checks.join(", ")
            }
        ));
    }
    if !step.checks.is_empty() && share.access < Access::Full {
        return Some(format!(
            "verify= needs full access, you have {} in '{folder}' on {}",
            share.access, peer.name
        ));
    }
    if step.apply.is_some() && !(share.worktrees && share.access >= Access::WorkspaceWrite) {
        return Some(format!(
            "apply= needs edit access in a folder shared with --worktrees; '{folder}' on {} isn't",
            peer.name
        ));
    }
    None
}

/// Checks `plan` against what is known now (connected peers, their folders, checks
/// and access, squads, knowledge and skills) so problems show before anything runs.
pub fn preflight(
    plan: &Plan,
    peers: &[PeerInfo],
    squads: &SquadStore,
    kb: &KnowledgeBase,
    skills: &Skills,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut report = |step: Option<&Step>, error: bool, message: String| {
        problems.push(Problem {
            step: step.map(|s| s.id.clone()),
            error,
            message,
        })
    };
    for step in &plan.steps {
        if let Err(e) = skills.instructions(&step.skills) {
            report(Some(step), true, format!("{e:#}"));
        }
        if let Err(e) = check_topics(kb, &step.context) {
            report(Some(step), true, format!("{e:#}"));
        }
        let candidates: Vec<&PeerInfo> = match &step.target {
            Target::Peer(name) if name == "me" => {
                if !step.approve {
                    report(
                        Some(step),
                        true,
                        "@me can only approve (approve @me); tasks run on peers".into(),
                    );
                }
                continue;
            }
            Target::Peer(name) => {
                let found: Vec<&PeerInfo> = peers.iter().filter(|p| &p.name == name).collect();
                if found.is_empty() {
                    report(Some(step), true, format!("{name} is not connected"));
                    continue;
                }
                found
            }
            Target::Squad(name) => match squads.get(name) {
                None => {
                    report(Some(step), true, format!("there is no squad #{name}"));
                    continue;
                }
                Some(squad) => {
                    let online: Vec<&PeerInfo> = peers
                        .iter()
                        .filter(|p| squad.members.contains(&p.name))
                        .collect();
                    if online.is_empty() {
                        report(Some(step), true, format!("no member of #{name} is online"));
                        continue;
                    }
                    online
                }
            },
        };
        if step.approve {
            continue;
        }
        let reasons: Vec<String> = candidates.iter().filter_map(|p| unable(step, p)).collect();
        if reasons.len() == candidates.len() {
            report(Some(step), true, reasons.join("; "));
        }
    }
    for claim in &plan.claims {
        let proving: Vec<&Step> = plan
            .steps
            .iter()
            .filter(|s| s.claims.contains(&claim.id))
            .collect();
        if proving.is_empty() {
            report(
                None,
                false,
                format!("no step proves claim {}: it will be unverified", claim.id),
            );
        } else if proving.iter().any(|s| !s.approve && s.checks.is_empty()) {
            report(
                None,
                false,
                format!(
                    "claim {} is claimed by a step without checks (verify=): that step can only leave it unverified",
                    claim.id
                ),
            );
        }
    }
    problems
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
    let mut running: JoinSet<(String, String, Result<Delivered>)> = JoinSet::new();
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
                let deps: Vec<(&str, &str, &str)> = step
                    .after
                    .iter()
                    .filter_map(|d| match &states[d] {
                        StepState::Done { peer, output, .. } => {
                            Some((d.as_str(), peer.as_str(), output.as_str()))
                        }
                        _ => None,
                    })
                    .collect();
                let prompt = step_prompt(step, &deps);
                let chosen = if step.approve {
                    choose_approver(step, &agent.peers()).map(|(id, name)| (id, name, None))
                } else {
                    choose_peer(step, &agent.peers(), squads, &busy).map(|peer| {
                        let mut task = LlmTask::new(step.provider, prompt.clone())
                            .with_checks(step.checks.clone(), step.attempts);
                        if let Some(share) = &step.share {
                            task = task.in_share(share.clone());
                        }
                        task.patch = step.apply.as_ref().and_then(|a| match &states[a] {
                            StepState::Done { patch, .. } => patch.clone(),
                            _ => None,
                        });
                        (Some(peer.id), peer.name, Some(task))
                    })
                };
                let (peer_id, peer_name, task) = match chosen {
                    Ok(chosen) => chosen,
                    Err(e) => {
                        let state = StepState::failed(None, format!("{e:#}"));
                        on_update(&step.id, &state);
                        states.insert(step.id.clone(), state);
                        changed = true;
                        continue;
                    }
                };
                if task
                    .as_ref()
                    .is_some_and(|t| step.apply.is_some() && t.patch.is_none())
                {
                    let state = StepState::failed(
                        None,
                        format!(
                            "step {} left no patch to apply (it changed nothing, or its folder isn't shared with --worktrees)",
                            step.apply.as_deref().unwrap_or_default()
                        ),
                    );
                    on_update(&step.id, &state);
                    states.insert(step.id.clone(), state);
                    changed = true;
                    continue;
                }
                *busy.entry(peer_name.clone()).or_default() += 1;
                let state = StepState::Running {
                    peer: peer_name.clone(),
                };
                on_update(&step.id, &state);
                states.insert(step.id.clone(), state);
                let (agent, id, title) = (
                    agent.clone(),
                    step.id.clone(),
                    format!("Plan step {}", step.id),
                );
                running.spawn(async move {
                    let result = match (peer_id, task) {
                        (Some(peer), Some(task)) => agent.delegate_verified(peer, task).await,
                        (approver, _) => agent.request_approval(approver, &title, &prompt).await,
                    };
                    (id, peer_name, result)
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
        let step = plan.steps.iter().find(|s| s.id == id).expect("known step");
        let state = match result {
            Ok(delivered) => StepState::from_delivered(peer, step, delivered),
            Err(e) => StepState::failed(Some(peer), format!("{e:#}")),
        };
        on_update(&id, &state);
        states.insert(id, state);
    }
    debug_assert!(states.values().all(StepState::finished));
    states
}

/// The person who must approve `step`: a connected peer by name, or `@me` (None).
fn choose_approver(step: &Step, peers: &[PeerInfo]) -> Result<(Option<iroh::EndpointId>, String)> {
    match &step.target {
        Target::Peer(name) if name == "me" => Ok((None, "you".to_owned())),
        Target::Peer(name) => peers
            .iter()
            .find(|p| &p.name == name)
            .map(|p| (Some(p.id), p.name.clone()))
            .ok_or_else(|| anyhow!("{name} is not connected")),
        Target::Squad(_) => bail!("approvals need a person: @name or @me"),
    }
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
    fn parses_claims_checks_patches_and_approvals() {
        let plan = Plan::parse(
            "claim: C-1 The parser handles empty input\n\
             impl @dev:app using claude verify=test,lint attempts=3 claims=C-1 Fix the parser\n\
             check after impl #qa:app apply=impl verify=test claims=C-1 Review it\n\
             signoff after check approve @alice claims=C-1 Approve the release\n\
             note @bob verify the=prompt words stay in the prompt",
        )
        .unwrap();
        assert_eq!(
            plan.claims,
            [ClaimDef {
                id: "C-1".into(),
                text: "The parser handles empty input".into()
            }]
        );
        let impl_ = &plan.steps[0];
        assert_eq!(impl_.provider, Provider::Claude);
        assert_eq!(impl_.checks, ["test", "lint"]);
        assert_eq!(impl_.attempts, Some(3));
        assert_eq!(impl_.claims, ["C-1"]);
        assert_eq!(impl_.prompt, "Fix the parser");
        assert_eq!(plan.steps[1].apply.as_deref(), Some("impl"));
        let signoff = &plan.steps[2];
        assert!(signoff.approve && !impl_.approve);
        assert_eq!(signoff.target, Target::Peer("alice".into()));
        assert_eq!(signoff.prompt, "Approve the release");
        assert_eq!(
            plan.steps[3].prompt,
            "verify the=prompt words stay in the prompt"
        );

        let fails = |text: &str, expected: &str| {
            let err = Plan::parse(text).unwrap_err().to_string();
            assert!(err.contains(expected), "{text:?}: {err}");
        };
        fails("a @x claims=C-9 do", "unknown claim 'C-9'");
        fails("a @x do\nb @x apply=a check", "must run after it");
        fails("a @x attempts=2 do", "no checks to retry on");
        fails(
            "a @x verify=t attempts=9 do",
            "attempts must be between 1 and 5",
        );
        fails("a approve #qa ok?", "needs a person");
        fails(
            "a approve @x:repo ok?",
            "can't have checks, a folder or a patch",
        );
        fails("claim: C-1\na @x do", "needs a statement");
        fails(
            "claim: C-1 x\nclaim: C-1 y\na @x do",
            "claim 'C-1' is defined twice",
        );
        fails("a approve @me save=notes ok?", "no result to save");
        fails("a @x save=../up do", "save= needs a topic name");
    }

    #[test]
    fn steps_use_and_grow_the_knowledge_base() {
        let dir = tempfile::tempdir().unwrap();
        let kb = KnowledgeBase::new(dir.path());
        kb.write("architecture", "Three services.", "").unwrap();
        let plan = Plan::parse(
            "design @x context=architecture save=design Design it\nplain @x save=other Do it\nlocal @x save=mine Note it",
        )
        .unwrap();
        let ready = plan.with_knowledge(&kb).unwrap();
        assert!(
            ready.steps[0]
                .prompt
                .contains("### Knowledge: architecture\nThree services.")
        );
        assert!(ready.steps[0].prompt.ends_with("---\nDesign it"));
        assert_eq!(ready.steps[1].prompt, "Do it");
        let missing = Plan::parse("a @x context=nope do").unwrap();
        assert!(
            missing
                .with_knowledge(&kb)
                .unwrap_err()
                .to_string()
                .contains("step 'a'")
        );

        let done = |verified| StepState::Done {
            peer: "x".into(),
            output: "The design.".into(),
            evidence: None,
            verified,
            patch: None,
        };
        let states: BTreeMap<String, StepState> = [
            ("design".to_owned(), done(true)),
            ("plain".to_owned(), done(false)),
            ("local".to_owned(), StepState::failed(None, "no".into())),
        ]
        .into();
        assert_eq!(save_knowledge(&kb, &plan, &states, "r1"), ["design"]);
        let saved = kb.read("design").unwrap();
        assert!(
            saved.contains("source: run r1, step design by x") && saved.contains("The design.")
        );
        assert!(
            kb.read("other").is_err(),
            "unverified results aren't knowledge"
        );
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
    fn steps_follow_named_skills() {
        let dir = tempfile::tempdir().unwrap();
        let kb = KnowledgeBase::new(dir.path().join("kb"));
        let skills = Skills::new(dir.path().join("skills"));
        let plan =
            Plan::parse("review @x skill=security-review,code-review Review the diff").unwrap();
        assert_eq!(plan.steps[0].skills, ["security-review", "code-review"]);
        let ready = plan.prepare(&kb, &skills).unwrap();
        let prompt = &ready.steps[0].prompt;
        assert!(prompt.starts_with("Follow these instructions:\n\n### Skill: security-review"));
        assert!(
            prompt.contains("### Skill: code-review") && prompt.ends_with("---\nReview the diff")
        );
        let unknown = Plan::parse("a @x skill=nope do").unwrap();
        assert!(
            unknown
                .prepare(&kb, &skills)
                .unwrap_err()
                .to_string()
                .contains("step 'a'")
        );
    }

    #[test]
    fn preflight_finds_what_would_fail_before_anything_runs() {
        let dir = tempfile::tempdir().unwrap();
        let kb = KnowledgeBase::new(dir.path().join("kb"));
        let skills = Skills::new(dir.path().join("skills"));
        let share = |name: &str, access: Access, checks: &[&str], worktrees: bool| ShareInfo {
            name: name.into(),
            access,
            checks: checks.iter().map(|c| (*c).to_owned()).collect(),
            worktrees,
        };
        let peer = |name: &str, shares: Vec<ShareInfo>| PeerInfo {
            name: name.into(),
            id: iroh::SecretKey::generate().public(),
            capabilities: vec![Provider::Copilot],
            shares,
            owner: None,
            role: None,
        };
        let peers = vec![
            peer("alice", vec![share("app", Access::Full, &["test"], true)]),
            peer(
                "bob",
                vec![share("app", Access::WorkspaceWrite, &["test"], false)],
            ),
        ];
        let mut squads = SquadStore::default();
        squads
            .set("qa", vec!["bob".into(), "carol".into()])
            .unwrap();
        let check = |text: &str| {
            preflight(&Plan::parse(text).unwrap(), &peers, &squads, &kb, &skills)
                .into_iter()
                .map(|p| {
                    format!(
                        "{}{}: {}",
                        if p.error { "E " } else { "W " },
                        p.step.unwrap_or_default(),
                        p.message
                    )
                })
                .collect::<Vec<_>>()
        };
        let good = "claim: C-1 tests pass\nfix @alice:app verify=test skill=implement-change claims=C-1 Fix it\nok after fix approve @me claims=C-1 Approve";
        assert!(check(good).is_empty(), "{:?}", check(good));

        let bad = check(
            "a @zed do\nb @alice:web do\nc @alice:app verify=lint do\nd @bob:app verify=test do\n\
             e after c #qa:app apply=c do\nf #ops do\ng @me do\nh @alice:app skill=nope context=gone do",
        );
        let expect = [
            "E a: zed is not connected",
            "E b: alice shares no folder 'web' with you (it shares: app)",
            "E c: folder 'app' on alice has no check lint (it has: test)",
            "E d: verify= needs full access, you have workspace-write in 'app' on bob",
            "E e: apply= needs edit access in a folder shared with --worktrees; 'app' on bob isn't",
            "E f: there is no squad #ops",
            "E g: @me can only approve",
            "E h: there is no skill 'nope'",
            "E h: the knowledge base has no topic 'gone'",
        ];
        for e in expect {
            assert!(
                bad.iter().any(|b| b.starts_with(e)),
                "missing {e:?} in {bad:#?}"
            );
        }
        let claims = check("claim: C-1 x\nclaim: C-2 y\na @alice:app claims=C-1 do");
        assert!(
            claims
                .iter()
                .any(|c| c.starts_with("W : no step proves claim C-2")),
            "{claims:?}"
        );
        assert!(
            claims
                .iter()
                .any(|c| c.contains("claim C-1 is claimed by a step without checks")),
            "{claims:?}"
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
                    ..Default::default()
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
