use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use p2pagents::audit::{AuditLog, Outcome};
use p2pagents::auth::{LoginMethod, ProviderTools};
use p2pagents::plan::{Plan, SquadStore, StepState, run_plan};
use p2pagents::profile::DEFAULT_GITHUB_HOST;
use p2pagents::trust::DEFAULT_TASKS_PER_HOUR;
use p2pagents::{
    Access, Agent, AgentConfig, Event, Executor, ExecutorConfig, LlmTask, Network, Profile,
    Provider, SdkExecutor, TaskContext, Ticket, TrustedPeer,
};
use tokio::io::{AsyncBufReadExt, BufReader};

const COMMANDS: &str = "\
commands:
  <text>                                broadcast a chat message
  /ask <provider> <prompt>              run a task locally (provider: copilot|claude|codex)
  /delegate <provider>[@peer[:folder]] <prompt>  run a task on a peer, in a folder it shares with you (default: data)
  /tasks                                list your running tasks
  /cancel <n>                           cancel your running task n (the peer stops it too)
  /plan <file>                          run a plan: steps on peers (@name) or squads (#squad), in dependency order
  /runs                                 list saved evidence runs (p2pagents runs show <id> for a report)
  /approvals                            list approval requests waiting for you
  /approve <n> [note], /reject <n> <why>  answer approval request n (signed with your key)
  /squads                               list your squads and which members are online
  /peers                                list connected peers and the providers they serve
  /ticket                               print this agent's ticket (give it to peers)
  /help                                 show this help
  /quit                                 exit

Each agent has its own identity (key) and Copilot/Claude/Codex accounts from its profile
(--profile, default: --name). Peers must allowlist each other's keys:  p2pagents peers --help
Sign a profile in to providers:  p2pagents auth --help";

/// Run a peer-to-peer agent that can run and delegate LLM coding tasks.
#[derive(Parser)]
#[command(version, after_help = COMMANDS, args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Sign agent profiles in/out of Copilot, Claude and Codex (each profile has its own accounts).
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Manage which peers (public keys) a profile's agents talk to and accept tasks from.
    #[command(subcommand)]
    Peers(PeersCommand),
    /// Manage the folders a profile shares with peers for their tasks.
    #[command(subcommand)]
    Share(ShareCommand),
    /// MCP servers (e.g. a browser, a docs or ticket system) a shared folder offers
    /// to peers' tasks: you define them, peers only learn their names.
    #[command(subcommand)]
    Mcp(McpCommand),
    /// Plugins: skills and MCP servers packaged together (GitHub Copilot CLI, Claude
    /// Code or Agent Plugins layouts, or one picked from a marketplace). Importing
    /// runs nothing: you offer a plugin's MCP servers in a folder when you choose.
    #[command(subcommand)]
    Plugin(PluginCommand),
    /// Create digital workers: agents with their own peer identity that run on
    /// their owner's accounts.
    #[command(subcommand)]
    Worker(WorkerCommand),
    /// Group peers into squads that plans can assign steps to.
    #[command(subcommand)]
    Squad(SquadCommand),
    /// Evidence runs: saved plan runs with a verdict per claim and signed evidence.
    #[command(subcommand)]
    Runs(RunsCommand),
    /// The knowledge base: what agents learned about your systems (topics, TSGs).
    #[command(subcommand)]
    Kb(KbCommand),
    /// Ready-made plans: bootstrap, discover, sdlc, learn, spec, design, implement-spec, rehearse.
    #[command(subcommand)]
    Playbook(PlaybookCommand),
    /// Skills: instructions agents follow in plan steps (skill=name). Built-in ones
    /// ship with p2pagents; a profile's own replace or add to them.
    #[command(subcommand)]
    Skill(SkillCommand),
    /// Spec-driven development: check a feature's spec, REST API contract and UX
    /// prototype, its design and its review pack (the checks plan steps run as
    /// verify=spec:api,design:threats,...).
    #[command(subcommand)]
    Spec(SpecCommand),
    /// The design package (architecture, ADRs, diagrams, data dictionary, data flows,
    /// threat model, test and operations plans) and the review pack (decks per
    /// audience, customer feedback document).
    #[command(subcommand)]
    Design(DesignCommand),
    /// Sign a profile in to GitHub (an account of the GitHub CLI) for repositories.
    #[command(subcommand)]
    Github(GithubCommand),
    /// Link GitHub repositories: clone, share with peers, sync, open pull requests.
    #[command(subcommand)]
    Repo(RepoCommand),
    /// Monitors: a folder's check run on a schedule; failures open incidents that
    /// TSGs or the debugging agent try to fix.
    #[command(subcommand)]
    Monitor(MonitorCommand),
    /// Scheduled plans: plans or playbooks the agent runs on its own at set times
    /// (each run is an evidence run).
    #[command(subcommand)]
    Schedule(ScheduleCommand),
    /// Operations: health report and incidents.
    #[command(subcommand)]
    Ops(OpsCommand),
    /// Manage profiles.
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// Install the Claude/Codex bridge (Node packages) into ~/.p2pagents/bridge.
    /// Not needed for Copilot-only agents.
    Setup {
        /// npm registry/mirror to install from (default: your npm config, usually registry.npmjs.org).
        #[arg(long)]
        registry: Option<String>,
    },
    /// Check this machine for everything agents need (CLIs, bridge, internet relay, profiles).
    Doctor {
        #[command(flatten)]
        bridge: BridgeArg,
    },
    /// Show the tasks peers ran on a profile's agents (who, provider, access, outcome).
    /// Prompts are not stored, only their SHA-256 and length.
    Audit {
        #[command(flatten)]
        profile: ProfileArg,
        /// How many of the most recent entries to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Print the raw JSON lines.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ProfileCommand {
    /// Delete a profile: its identity key, stored credentials, allowlist and provider sign-ins.
    Delete {
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum WorkerCommand {
    /// Create a digital worker owned by --owner.
    #[command(after_help = "\
examples:
  p2pagents worker create tester --owner alice --role \"test engineer for the parser\"
  p2pagents --name tester --providers copilot --serve        (runs on alice's Copilot sign-in)
The worker gets its own key, allowlist, shared folders and audit log, but no accounts:
it always uses its owner's. Its owner is allowlisted on it (may run read-only tasks in its
data folder) and it is allowlisted on its owner (chat only).")]
    Create {
        /// Name of the new worker (also its profile name).
        name: String,
        /// Profile that owns the worker and whose accounts it uses.
        #[arg(long)]
        owner: String,
        /// What the worker is for; shown to peers and given to the model with every task.
        #[arg(long)]
        role: Option<String>,
    },
    /// List digital workers (all, or those of --owner).
    List {
        #[arg(long)]
        owner: Option<String>,
    },
}

#[derive(Subcommand)]
enum SquadCommand {
    /// Create or replace a squad of allowlisted peers.
    #[command(after_help = "\
examples:
  p2pagents squad create qa --member alice,tester --profile boss
In a plan, `#qa` runs a step on the least busy online member of squad qa.")]
    Create {
        name: String,
        /// Allowlist names of the members (comma-separated).
        #[arg(long, value_delimiter = ',', required = true)]
        member: Vec<String>,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// List squads.
    List {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Remove a squad.
    Remove {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum KbCommand {
    /// List topics (troubleshooting guides are marked).
    List {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Print a topic.
    Show {
        topic: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Add or replace a topic from a Markdown file (`-` for stdin).
    Add {
        topic: String,
        file: PathBuf,
        #[command(flatten)]
        profile: ProfileArg,
    },
    Remove {
        topic: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Topics containing all the words.
    Search {
        #[arg(required = true)]
        words: Vec<String>,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Learn from how things ran: write the ops report (health, incidents, tasks,
    /// runs) into the topic `ops-summary`, for the learn playbook and context=all.
    Learn {
        #[arg(long, default_value_t = 168)]
        hours: u64,
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum GithubCommand {
    /// Use a GitHub CLI account (see `gh auth status`) for this profile's repositories.
    Login {
        #[arg(long)]
        gh_user: String,
        #[arg(long, default_value = DEFAULT_GITHUB_HOST)]
        host: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Show the profile's GitHub account and the GitHub CLI accounts on this machine.
    Status {
        #[command(flatten)]
        profile: ProfileArg,
    },
    Logout {
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum RepoCommand {
    /// Clone a repository with the profile's GitHub account and share it with peers
    /// (a git worktree per task).
    #[command(after_help = "\
examples:
  p2pagents github login --gh-user alice --profile alice
  p2pagents repo link acme/shop --access full --check test=\"npm test\" --check lint=\"npm run lint\" --profile alice
  p2pagents peers allow <boss-ticket> --profile alice --delegate --access full --share data,shop
Agents' tasks commit on p2p/... branches; review them with repo branches and open a
pull request with repo pr.")]
    Link {
        /// owner/repo, host/owner/repo or an https URL.
        repo: String,
        /// Folder name peers use (default: the repository name).
        #[arg(long = "as")]
        name: Option<String>,
        #[arg(long, value_enum, default_value_t = Access::WorkspaceWrite)]
        access: Access,
        #[arg(long = "check", value_name = "NAME=COMMAND")]
        checks: Vec<String>,
        #[command(flatten)]
        profile: ProfileArg,
    },
    List {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Fetch and fast-forward the clone.
    Sync {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Branches agents' tasks committed to, newest first.
    Branches {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Push a task branch and open a pull request into the repository's branch.
    Pr {
        name: String,
        branch: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, default_value = "")]
        body: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Stop sharing a repository and forget it (--delete also removes the clone).
    Unlink {
        name: String,
        #[arg(long)]
        delete: bool,
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum SkillCommand {
    /// List skills (built-in and the profile's own).
    List {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Print a skill.
    Show {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Add a skill, or replace a built-in one, from a Markdown file (`-` for stdin).
    #[command(after_help = "\
A skill file starts with front matter:
  ---
  name: security-review
  description: Our security review checklist
  ---
  # Security review
  ...
Use it in a plan step with skill=security-review.")]
    Add {
        name: String,
        file: PathBuf,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Remove a profile skill (a built-in one of the same name applies again).
    Remove {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Import a skill: a SKILL.md, a folder with one (e.g. from a skills repository)
    /// or a Markdown file. Its name comes from its front matter, else the file or folder.
    Import {
        path: PathBuf,
        /// Save it under another name.
        #[arg(long = "as", value_name = "NAME")]
        rename: Option<String>,
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum SpecCommand {
    /// Run built-in checks on a folder: a check (api, threats, review:decks, ...), a
    /// group (spec, design, review) or all.
    #[command(after_help = "\
checks:
  spec:spec spec:api spec:ux spec:trace              the spec package (spec/, prototype/, evidence/claims.json)
  design:arch design:data design:threats             the design package (design/)
  design:tests design:ops
  review:decks review:rehearsal                      the review pack (review/) and its rehearsal
'all' (the default) runs the spec checks, and the design and review checks when
design/ and review/ exist; name a check or a group (spec, design, review) to run it
anyway. Short names work too: api, threats, decks.
A spec package is:
  spec/SPEC.md          user stories (US-1: ...) and acceptance criteria (AC-1 (US-1): Given/When/Then)
  spec/openapi.json     the REST API contract; operations list their criteria in x-acceptance
  prototype/index.html  a self-contained clickable prototype; screens marked data-story/data-ac
  evidence/claims.json  a claim per acceptance criterion, proven later by tests
examples:
  p2pagents spec check
  p2pagents spec check api --dir D:\\src\\shop
  p2pagents spec check design")]
    Check {
        /// A check, a group (spec, design, review) or all.
        #[arg(default_value = "all")]
        part: String,
        /// The folder holding the package.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Create a starter spec package (an example that passes every check) to edit.
    Init {
        /// The feature's title.
        title: String,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Replace an existing spec package.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum DesignCommand {
    /// Create a starter design package and review pack (an example that, with the
    /// spec starter, passes every check) to edit.
    #[command(after_help = "\
It writes:
  design/architecture.md      context, components, domain model, deployment, quality attributes, decisions
  design/workflows.md         UML sequence and state diagrams, a workflow per user story
  design/adr/ADR-0001-*.md    architecture decision records
  design/data-dictionary.md   every field with type, classification, purpose and retention
  design/dataflow.md          the data flow diagram with trust boundaries, and the flows (DF-1)
  design/threat-model.md      STRIDE threats (TM-1) with mitigations, risk and status
  design/test-plan.md         a test for every acceptance criterion and high-risk threat
  design/operations.md        rollout, rollback, SLOs, metrics, dashboards, alerts, runbooks
  review/technical.md         technical review deck (Marp)
  review/product.md           product review deck (Marp)
  review/leadership.md        leadership review deck (Marp)
  review/customer-feedback.md customer feedback document
Check them with: p2pagents spec check design (and review)")]
    Init {
        /// The feature's title.
        title: String,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Replace an existing design package.
        #[arg(long)]
        force: bool,
    },
    /// Render the review pack and design documents as self-contained HTML: decks as
    /// slide shows (speaker notes, print to PDF), documents as pages.
    #[command(after_help = "\
Open <out>/index.html in a browser. For PowerPoint, the decks are Marp Markdown:
  npx @marp-team/marp-cli review/leadership.md --pptx")]
    Render {
        /// The folder holding design/, review/ and prototype/.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Where to write the HTML (default: <dir>/review/html).
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum PlaybookCommand {
    /// List the playbooks and what they need.
    List,
    /// Explain what a playbook does, step by step, and what you need to run it.
    Explain { name: String },
    /// Print a playbook as a plan, filled in with --set values.
    #[command(after_help = "\
examples:
  p2pagents playbook show discover --set peer=alice --set folder=app --set project=Shop > discover.plan
  p2pagents playbook show sdlc --set peer=alice --set change=\"Add CSV export\" --set change_id=csv
Then run it in an agent with /plan <file>, or pick the playbook in the web UI's Plan mode.")]
    Show {
        name: String,
        /// A value for a placeholder: NAME=VALUE (repeatable).
        #[arg(long = "set", value_name = "NAME=VALUE")]
        set: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ScheduleCommand {
    /// Run a plan file or a playbook at set times (replaces a schedule of that name).
    #[command(after_help = "\
when (local time): 'every 30m', 'every 2h', 'daily 09:00', 'weekdays 08:30', 'mon,fri 17:00'
examples:
  p2pagents schedule add nightly-learn --when \"daily 02:00\" --playbook learn --set peer=alice --set folder=app --profile boss
  p2pagents schedule add smoke --when \"every 1h\" --plan smoke.plan --profile boss
Schedules run while the profile's agent runs (with or without --ui), never overlap
themselves, and each run is saved as an evidence run. A run missed while the agent
was off runs once when it starts again.")]
    Add {
        name: String,
        /// When to run, e.g. "weekdays 09:00" or "every 2h".
        #[arg(long)]
        when: String,
        /// A plan file to run.
        #[arg(long, conflicts_with = "playbook")]
        plan: Option<PathBuf>,
        /// A playbook to run, filled in with --set values.
        #[arg(long)]
        playbook: Option<String>,
        /// A value for a playbook placeholder: NAME=VALUE (repeatable).
        #[arg(long = "set", value_name = "NAME=VALUE", requires = "playbook")]
        set: Vec<String>,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// List a profile's schedules, their next run and last verdict.
    List {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Stop running a schedule until resumed.
    Pause {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Run a paused schedule again (its next run counts from now).
    Resume {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Delete a schedule.
    Remove {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum MonitorCommand {
    /// Add or replace a monitor (runs in this profile's agent while it is running).
    #[command(after_help = "\
example:
  p2pagents share add app D:\\src\\app --access full --check health=\"curl -fsS http://localhost:8080/health\" --check restart=\"pwsh -File restart.ps1\" --profile alice
  p2pagents monitor add api --share app --check health --every 60 --slo 99.5 --profile alice")]
    Add {
        name: String,
        /// Shared folder whose check to run (fixes run there too).
        #[arg(long)]
        share: String,
        #[arg(long)]
        check: String,
        /// Seconds between runs (at least 10).
        #[arg(long, default_value_t = 60)]
        every: u64,
        /// Failures in a row that open an incident.
        #[arg(long, default_value_t = 3)]
        fail_after: u32,
        /// Availability objective, percent.
        #[arg(long, default_value_t = 99.0)]
        slo: f64,
        #[command(flatten)]
        profile: ProfileArg,
    },
    List {
        #[command(flatten)]
        profile: ProfileArg,
    },
    Remove {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum OpsCommand {
    /// Health against SLOs, incidents, peer tasks and evidence runs (Markdown).
    Report {
        #[arg(long, default_value_t = 24)]
        hours: u64,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Incident events, oldest first.
    Incidents {
        #[arg(long, default_value_t = 24)]
        hours: u64,
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum RunsCommand {
    /// List saved runs, newest first.
    List {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Print a run's evidence report (Markdown).
    Show {
        id: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Re-check every signature in a run, from its run.json alone; exits 1 if any fails.
    #[command(after_help = "\
Works on a run folder copied from someone else too:  p2pagents runs verify --dir <folder>")]
    Verify {
        /// Run id (in the profile's runs folder).
        #[arg(required_unless_present = "dir")]
        id: Option<String>,
        /// A run folder (with run.json) from anywhere.
        #[arg(long, conflicts_with = "id")]
        dir: Option<PathBuf>,
        /// Profile whose runs folder holds the run (with a run id).
        #[arg(long = "profile", required_unless_present = "dir")]
        profile: Option<String>,
    },
}

#[derive(Subcommand)]
enum ShareCommand {
    /// Share a folder with peers (grant it per peer with `peers allow --share`).
    #[command(after_help = "\
examples:
  p2pagents share add myrepo D:\\src\\myrepo --access workspace-write --profile alice
  p2pagents share add app D:\\src\\app --access full --worktrees --check test=\"cargo test\" --check lint=\"cargo clippy -- -D warnings\"
  p2pagents peers allow <bob-ticket> --profile alice --delegate --access workspace-write --share data,myrepo
Every profile also shares its own .p2pdata folder as 'data' (read-only); peers'
tasks run there unless they name another folder shared with them.
Checks are commands you define; a peer's task can ask your agent to run them after the
model (verify=test in a plan) as signed evidence. They run your folder's code with your
rights, so only peers with full access may request them.")]
    Add {
        /// Name peers use for the folder (e.g. `/delegate copilot@alice:myrepo ...`).
        name: String,
        /// Existing folder to share.
        path: PathBuf,
        /// Most any peer's task may do in this folder.
        #[arg(long, value_enum, default_value_t = Access::ReadOnly)]
        access: Access,
        /// A check peers' tasks may ask for: NAME=COMMAND (repeatable).
        #[arg(long = "check", value_name = "NAME=COMMAND")]
        checks: Vec<String>,
        /// Run each editing task in its own git worktree and branch, and commit its work
        /// (the folder must be a git repository).
        #[arg(long)]
        worktrees: bool,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// List the folders a profile shares.
    List {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Stop sharing a folder.
    Remove {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Offer an MCP server to peers' tasks in a shared folder (they ask for it with
    /// mcp=NAME in a plan step).
    #[command(after_help = "\
examples:
  p2pagents mcp add app browser --access read-only --tool browser_navigate --tool browser_snapshot -- npx @playwright/mcp@latest --headless
  p2pagents mcp add app docs --url https://docs.example.com/mcp --header \"Authorization=Bearer <token>\"
  p2pagents mcp add app tickets --plugin team-tools --set TICKETS_TOKEN=<token> --description \"Our ticket system\"
  plan step:  check @alice:app mcp=browser Open http://localhost:3000 and report what the home page shows
The server runs on this machine with your rights; commands, variables, headers and
URLs stay here (peers learn only the names they may use, and descriptions you give).
Tasks call only the tools you list (default: all), and tool results reach the model
marked as untrusted content. A plugin server's values not given with --set are
taken from environment variables of the same name.")]
    Add {
        /// The shared folder whose tasks may use the server.
        share: String,
        /// Name tasks use for the server.
        name: String,
        /// A remote server's URL (https://, or http:// on this machine).
        #[arg(long)]
        url: Option<String>,
        /// A header for the remote server: NAME=VALUE (repeatable).
        #[arg(long = "header", value_name = "NAME=VALUE", requires = "url")]
        headers: Vec<String>,
        /// The remote server uses Server-Sent Events.
        #[arg(long, requires = "url")]
        sse: bool,
        /// An environment variable for a local server: NAME=VALUE (repeatable).
        #[arg(long = "env", value_name = "NAME=VALUE")]
        env: Vec<String>,
        /// Make the server from an imported plugin (see `plugin show`).
        #[arg(long, conflicts_with_all = ["url", "command", "env"])]
        plugin: Option<String>,
        /// The plugin's server (default: the one called NAME, or its only one).
        #[arg(long, requires = "plugin")]
        server: Option<String>,
        /// A value the plugin's server needs: NAME=VALUE (repeatable).
        #[arg(long = "set", value_name = "NAME=VALUE", requires = "plugin")]
        set: Vec<String>,
        /// A tool tasks may call (repeatable; default: all of the server's tools).
        #[arg(long = "tool", value_name = "TOOL")]
        tools: Vec<String>,
        /// The least access a peer's task needs to use the server.
        #[arg(long, value_enum, default_value_t = Access::Full)]
        access: Access,
        /// What the server is for, in a line (peers see it next to its name).
        #[arg(long)]
        description: Option<String>,
        #[command(flatten)]
        profile: ProfileArg,
        /// A local server: its command and arguments, after `--`.
        #[arg(last = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// Offer the servers of an MCP configuration file (mcp.json, .mcp.json,
    /// .vscode/mcp.json) in a shared folder.
    #[command(after_help = "\
examples:
  p2pagents mcp import .vscode/mcp.json --share app --server playwright --access read-only
  p2pagents mcp import mcp.json --share app --set API_KEY=<key>
${workspaceFolder} becomes the shared folder. Values the servers need (${input:x},
${env:X}) come from --set, else environment variables of the same name. Disabled
servers and WebSocket servers are left out.")]
    Import {
        file: PathBuf,
        /// The shared folder whose tasks may use the servers.
        #[arg(long)]
        share: String,
        /// Only this server (repeatable; default: all of them).
        #[arg(long = "server", value_name = "NAME")]
        servers: Vec<String>,
        /// A value the servers need: NAME=VALUE (repeatable).
        #[arg(long = "set", value_name = "NAME=VALUE")]
        set: Vec<String>,
        /// The least access a peer's task needs to use them.
        #[arg(long, value_enum, default_value_t = Access::Full)]
        access: Access,
        /// Replace servers the folder offers under the same names.
        #[arg(long)]
        replace: bool,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// List the MCP servers a profile's shared folders offer (secrets hidden).
    List {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Stop offering an MCP server for now; it keeps its settings.
    Pause {
        share: String,
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Offer a paused MCP server again.
    Resume {
        share: String,
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Stop offering an MCP server.
    Remove {
        share: String,
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Import a plugin from a folder, a GitHub repository or a git URL. Nothing in it
    /// runs: its skills become usable in plan steps, its MCP servers become templates
    /// you can offer in a folder (mcp add --plugin).
    #[command(after_help = "\
examples:
  p2pagents plugin add ./team-tools
  p2pagents plugin add octo-org/agent-plugins:plugins/reviewer
  p2pagents plugin add octo-org/marketplace --plugin reviewer
  p2pagents plugin add https://git.example.com/tools.git#plugins/qa --as qa-tools
Private GitHub repositories use the profile's GitHub sign-in (p2pagents github login).")]
    Add {
        /// A folder, owner/repo[:path], host/owner/repo[:path] or a git URL (#path for a folder in it).
        source: String,
        /// The plugin to import, when the source is a marketplace.
        #[arg(long)]
        plugin: Option<String>,
        /// Import it under another name.
        #[arg(long = "as", value_name = "NAME")]
        rename: Option<String>,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// List imported plugins.
    List {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Show what a plugin contains and where its MCP servers are offered.
    Show {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Fetch a plugin again from where it came from. MCP servers that now start
    /// differently are paused until you review and resume them.
    Update {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Turn a plugin on: its skills and offered MCP servers apply again.
    Enable {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Turn a plugin off: its skills and MCP servers stop applying (settings kept).
    Disable {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Remove a plugin, its files and the MCP servers it offered.
    Remove {
        name: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum PeersCommand {
    /// Print this profile's endpoint id and a ticket to give to peers.
    Id {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Allowlist a peer by its ticket or endpoint id.
    #[command(after_help = "\
examples:
  p2pagents peers allow p2pa1eyJuYW1lIjoi... --profile alice --delegate
  p2pagents peers allow p2pa1eyJuYW1lIjoi... --profile alice --delegate --access workspace-write
  p2pagents peers allow 5f2c...e9 --profile alice --name bob --tasks-per-hour 5
Both sides must allowlist each other. Only peers allowed with --delegate can make this
agent run LLM tasks (on this profile's accounts), with at most their --access and the
agent's own --access.")]
    Allow {
        /// Peer's ticket (`p2pa1...`, from `peers id` or `/ticket`) or endpoint id.
        target: String,
        #[command(flatten)]
        profile: ProfileArg,
        /// Local name for the peer (default: the name in the ticket).
        #[arg(long)]
        name: Option<String>,
        /// Allow this peer to delegate LLM tasks to this profile's agents.
        #[arg(long)]
        delegate: bool,
        /// Maximum tasks this peer may delegate per hour.
        #[arg(long, default_value_t = DEFAULT_TASKS_PER_HOUR)]
        tasks_per_hour: u32,
        /// What this peer's tasks may do: read-only, workspace-write (edit files) or
        /// full (also shell commands). Capped by each shared folder's maximum.
        #[arg(long, value_enum, default_value_t = Access::ReadOnly, requires = "delegate")]
        access: Access,
        /// Shared folders this peer's tasks may use (see `p2pagents share`).
        #[arg(
            long,
            value_delimiter = ',',
            default_value = "data",
            requires = "delegate"
        )]
        share: Vec<String>,
    },
    /// List allowlisted peers.
    List {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Remove a peer from the allowlist.
    Remove {
        /// Peer name or endpoint id.
        peer: String,
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Sign a profile in to a provider.
    #[command(after_help = "\
examples:
  p2pagents auth login copilot --profile worker1 --gh-user alice
  p2pagents auth login copilot --profile worker2 --gh-user bob --host mycorp.ghe.com
  p2pagents auth login copilot --profile worker3 --token        (prompts for a GitHub token)
  p2pagents auth login claude  --profile worker1                 (browser sign-in)
  p2pagents auth login claude  --profile worker2 --api-key
  p2pagents auth login codex   --profile worker1                 (browser sign-in)
  p2pagents auth login codex   --profile worker2 --api-key
Secrets are read from a hidden prompt, or from stdin when piped.")]
    Login {
        #[arg(value_enum)]
        provider: Provider,
        #[command(flatten)]
        profile: ProfileArg,
        /// Copilot: use this GitHub CLI account's token (add accounts with `gh auth login`).
        #[arg(long, conflicts_with_all = ["token", "api_key"])]
        gh_user: Option<String>,
        /// Copilot: GitHub host of the account.
        #[arg(long, default_value = DEFAULT_GITHUB_HOST)]
        host: String,
        /// Copilot: a GitHub token. Claude: an OAuth token from `claude setup-token`.
        #[arg(long, conflicts_with = "api_key")]
        token: bool,
        /// Claude: Anthropic API key. Codex: OpenAI API key.
        #[arg(long)]
        api_key: bool,
        #[command(flatten)]
        bridge: BridgeArg,
    },
    /// Show which account each provider uses, for one profile or all profiles.
    Status {
        /// Profile to show (default: all profiles).
        #[arg(long)]
        profile: Option<String>,
        #[command(flatten)]
        bridge: BridgeArg,
    },
    /// Sign a profile out of a provider and delete its stored credentials.
    Logout {
        #[arg(value_enum)]
        provider: Provider,
        #[command(flatten)]
        profile: ProfileArg,
        #[command(flatten)]
        bridge: BridgeArg,
    },
}

#[derive(ClapArgs)]
struct ProfileArg {
    /// Agent profile (identity) to sign in.
    #[arg(long = "profile", id = "profile")]
    name: String,
}

#[derive(ClapArgs)]
struct BridgeArg {
    /// Node bridge script for the Claude and Codex SDKs (default: the one `setup` installed).
    #[arg(long, env = "P2PAGENTS_BRIDGE")]
    bridge: Option<PathBuf>,
}

impl BridgeArg {
    fn path(&self) -> PathBuf {
        p2pagents::setup::resolve_bridge(self.bridge.clone())
    }
}

#[derive(ClapArgs)]
struct RunArgs {
    /// Agent name shown to peers (required to run an agent).
    #[arg(long)]
    name: Option<String>,
    /// Profile whose provider instances and accounts this agent uses (default: --name).
    #[arg(long)]
    profile: Option<String>,
    /// How to reach peers: over the internet (relays, NAT traversal) or direct IPs only.
    #[arg(long, value_enum, default_value_t = Network::Internet)]
    network: Network,
    /// UDP address to bind (default: any port). Required with --network local.
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// Allowlisted peer to connect to on startup, as a ticket or endpoint id (repeatable).
    /// All allowlisted peers are also dialed automatically.
    #[arg(long = "peer")]
    peers: Vec<String>,
    /// LLM providers to enable on this agent (comma-separated).
    #[arg(long, value_enum, value_delimiter = ',')]
    providers: Vec<Provider>,
    /// Run tasks delegated by peers with the enabled providers.
    #[arg(long)]
    serve: bool,
    /// Folder your own tasks (/ask, "This agent" in the web UI) work in. Peers never
    /// get it: their tasks run only in folders you share (see `p2pagents share`).
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// Most your own tasks may do in --workspace: read-only, workspace-write (edit
    /// files) or full (also shell commands). Peers' access is set per peer and folder.
    #[arg(long, value_enum, default_value_t = Access::ReadOnly)]
    access: Access,
    /// Open the web UI (served on 127.0.0.1 only) instead of the terminal prompt.
    #[arg(long)]
    ui: bool,
    /// Port for the web UI (default: any free port).
    #[arg(long, default_value_t = 0, requires = "ui")]
    ui_port: u16,
    /// Don't open the web UI in the browser automatically; just print its link.
    #[arg(long, requires = "ui")]
    no_open: bool,
    #[command(flatten)]
    bridge: BridgeArg,
}

/// Opens `url` in the default browser.
fn open_browser(url: &str) {
    let result = if cfg!(windows) {
        // `start` treats the first quoted argument as a window title.
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if result.is_err() {
        eprintln!("could not open a browser; open the link above manually");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Auth(command)) => run_auth(command).await,
        Some(Command::Peers(command)) => run_peers(command),
        Some(Command::Share(command)) => run_share(command),
        Some(Command::Mcp(command)) => run_mcp(command),
        Some(Command::Plugin(command)) => run_plugin(command).await,
        Some(Command::Worker(command)) => run_worker(command),
        Some(Command::Squad(command)) => run_squad(command),
        Some(Command::Runs(command)) => run_runs(command),
        Some(Command::Kb(command)) => run_kb(command),
        Some(Command::Playbook(command)) => run_playbook(command),
        Some(Command::Skill(command)) => run_skill(command),
        Some(Command::Spec(command)) => run_spec(command),
        Some(Command::Design(command)) => run_design(command),
        Some(Command::Github(command)) => run_github(command),
        Some(Command::Repo(command)) => run_repo(command).await,
        Some(Command::Monitor(command)) => run_monitor(command),
        Some(Command::Schedule(command)) => run_schedule(command),
        Some(Command::Ops(command)) => run_ops(command),
        Some(Command::Profile(ProfileCommand::Delete { profile })) => {
            let profile = Profile::open(&profile.name)?;
            let name = profile.name.clone();
            profile.delete()?;
            println!("profile '{name}' deleted");
            Ok(())
        }
        Some(Command::Setup { registry }) => {
            let script = p2pagents::setup::install_bridge(registry.as_deref())?;
            println!("bridge installed: {}", script.display());
            println!(
                "claude and codex providers are ready; sign a profile in with: p2pagents auth login claude --profile <name>"
            );
            Ok(())
        }
        Some(Command::Doctor { bridge }) => {
            if !p2pagents::setup::doctor(&bridge.path()).await? {
                std::process::exit(1);
            }
            Ok(())
        }
        Some(Command::Audit {
            profile,
            limit,
            json,
        }) => run_audit(&profile.name, limit, json),
        None => run_agent(cli.run).await,
    }
}

fn run_audit(profile: &str, limit: usize, json: bool) -> Result<()> {
    let profile = Profile::open(profile)?;
    let entries = AuditLog::read(&profile.audit_path(), limit)?;
    if json {
        for entry in &entries {
            println!("{}", serde_json::to_string(entry)?);
        }
        return Ok(());
    }
    if entries.is_empty() {
        println!("profile '{}': no peer tasks recorded yet", profile.name);
        return Ok(());
    }
    println!(
        "{:<20} {:<14} {:>5} {:<8} {:<16} {:<8} {:>8}  prompt",
        "time (UTC)", "peer", "task", "provider", "access", "outcome", "seconds"
    );
    for e in &entries {
        let secs = e.ts_ms / 1000;
        let (days, rem) = (secs / 86_400, secs % 86_400);
        let access = e.access.map_or("-".to_owned(), |a| a.to_string());
        let outcome = serde_json::to_value(e.outcome)?;
        println!(
            "{:<20} {:<14} {:>5} {:<8} {:<16} {:<8} {:>8.1}  {} chars, sha256 {}{}",
            format!(
                "{} {:02}:{:02}:{:02}",
                civil_date(days),
                rem / 3600,
                rem % 3600 / 60,
                rem % 60
            ),
            e.peer,
            e.task_id,
            e.provider.to_string(),
            access,
            outcome.as_str().unwrap_or("?"),
            e.duration_ms as f64 / 1000.0,
            e.prompt_chars,
            &e.prompt_sha256[..12],
            e.error
                .as_deref()
                .map(|err| format!("\n{:>22}{err}", "error: "))
                .unwrap_or_default(),
        );
    }
    println!("log: {}", profile.audit_path().display());
    Ok(())
}

/// `YYYY-MM-DD` for days since 1970-01-01 (proleptic Gregorian calendar).
fn civil_date(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn run_worker(command: WorkerCommand) -> Result<()> {
    match command {
        WorkerCommand::Create { name, owner, role } => {
            let owner = Profile::open(&owner)?;
            let worker = owner.create_digital_worker(&name, role)?;
            // Owner and worker know each other: the owner may give it tasks.
            let (owner_id, worker_id) = (owner.identity()?.public(), worker.identity()?.public());
            let mut trust = worker.trust()?;
            trust.allow(TrustedPeer {
                name: owner.name.clone(),
                id: owner_id,
                addrs: Default::default(),
                can_delegate: true,
                tasks_per_hour: DEFAULT_TASKS_PER_HOUR,
                access: Access::ReadOnly,
                shares: vec![p2pagents::shares::DATA_SHARE.to_owned()],
            });
            trust.save(&worker.trust_path())?;
            let mut trust = owner.trust()?;
            trust.allow(TrustedPeer {
                name: worker.name.clone(),
                id: worker_id,
                addrs: Default::default(),
                can_delegate: false,
                tasks_per_hour: DEFAULT_TASKS_PER_HOUR,
                access: Access::ReadOnly,
                shares: vec![p2pagents::shares::DATA_SHARE.to_owned()],
            });
            trust.save(&owner.trust_path())?;
            println!(
                "digital worker '{name}' created for '{}' (endpoint id {worker_id}); it runs on '{}''s accounts",
                owner.name, owner.name
            );
            println!(
                "start it:  p2pagents --name {name} --providers copilot --serve\nshare its ticket with peers:  p2pagents peers id --profile {name}"
            );
        }
        WorkerCommand::List { owner } => {
            for name in Profile::list()? {
                let profile = Profile::open(&name)?;
                let Some(of) = &profile.config.owner else {
                    continue;
                };
                if owner.as_ref().is_some_and(|o| o != of) {
                    continue;
                }
                println!(
                    "  {name:<16} owner {of:<12} {}",
                    profile.config.role.as_deref().unwrap_or("")
                );
            }
        }
    }
    Ok(())
}

/// `; checks: test, lint; worktrees` for a share listing.
fn share_extras(share: &p2pagents::shares::Share) -> String {
    let mut extras = String::new();
    if !share.checks.is_empty() {
        extras += &format!(
            "; checks: {}",
            share
                .checks
                .iter()
                .map(|(name, cmd)| format!("{name} ({cmd})"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if share.worktrees {
        extras += "; a git worktree per editing task";
    }
    if !share.mcp.is_empty() {
        extras += &format!(
            "; MCP servers: {}",
            share.mcp.keys().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    extras
}

/// Runs the profile's scheduled plans in the background while the agent runs (the
/// web UI runs its own, so their steps show up there).
fn start_schedules(profile: &Profile, agent: &Agent) -> Result<()> {
    use p2pagents::schedule::{Launch, Outcome, Schedule, ScheduleStore, Scheduler};
    let store = ScheduleStore::load(&profile.schedules_path())?;
    if !store.schedules.is_empty() {
        println!(
            "schedules: {}",
            store
                .schedules
                .iter()
                .map(|s| format!(
                    "{} ({}{})",
                    s.name,
                    s.when,
                    if s.enabled { "" } else { ", paused" }
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let (agent, owner) = (agent.clone(), profile.clone());
    let launch: Launch = Arc::new(move |s: Schedule| {
        let ready = p2pagents::plan::start(&agent, &owner, &s.plan)?;
        let agent = agent.clone();
        Ok(Box::pin(async move {
            println!("=== scheduled plan '{}' started ({}) ===", s.name, s.when);
            let run = ready.run(&agent, print_step).await;
            println!(
                "=== scheduled plan '{}' finished: {} (runs show {}) ===",
                s.name,
                run.verdict_line(),
                run.id
            );
            Outcome {
                verdict: run.verdict_line(),
                run_id: Some(run.id),
            }
        }))
    });
    let tick = std::env::var("P2PAGENTS_SCHEDULE_TICK_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    Scheduler::new(profile.schedules_path(), launch).spawn(Duration::from_secs(tick));
    Ok(())
}

/// Runs the profile's monitors in the background (if any). Incidents are printed and
/// sent to connected peers as chat, so the team sees alerts.
fn start_monitors(
    profile: &Profile,
    agent: &Agent,
    executor: Option<Arc<dyn Executor>>,
) -> Result<()> {
    use p2pagents::ops::{IncidentEvent, IncidentKind, MonitorStore, Ops};
    let monitors = MonitorStore::load(&profile.monitors_path())?.monitors;
    if monitors.is_empty() {
        return Ok(());
    }
    println!(
        "monitors: {} (incidents try troubleshooting guides, then {})",
        monitors
            .iter()
            .map(|m| format!("{} every {}s", m.name, m.every_secs))
            .collect::<Vec<_>>()
            .join(", "),
        if executor.is_some() {
            "the debugging agent"
        } else {
            "a person (no --providers for a debugging agent)"
        }
    );
    let alerts = agent.clone();
    let name = agent.name().to_owned();
    let notify = move |e: &IncidentEvent| {
        let line = e.text.lines().next().unwrap_or_default();
        println!("! [{}] {:?}: {line}", e.monitor, e.kind);
        if matches!(
            e.kind,
            IncidentKind::Opened
                | IncidentKind::Escalated
                | IncidentKind::Resolved
                | IncidentKind::Learned
        ) {
            alerts.broadcast(format!(
                "[{name} alert] {} {}: {line}",
                e.monitor,
                format!("{:?}", e.kind).to_lowercase()
            ));
        }
    };
    let current = agent.clone();
    let ops = Arc::new(Ops {
        agent_name: agent.name().to_owned(),
        monitors,
        // As they are now: shares changed while the agent runs apply to its monitors.
        shares: Arc::new(move || current.shares()),
        store: profile.ops_store(),
        kb: profile.knowledge(),
        skills: profile.skills(),
        executor,
        notify: Arc::new(notify),
    });
    tokio::spawn(ops.run());
    // Keep learning: refresh the ops summary in the knowledge base every hour.
    let profile = profile.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            if let Ok(report) = ops_report(&profile, 168) {
                let _ = profile.knowledge().write(
                    "ops-summary",
                    &report,
                    "ops report of the last 168 hours (refreshed hourly)",
                );
            }
        }
    });
    Ok(())
}

/// The profile's ops report for the last `hours`.
fn ops_report(profile: &Profile, hours: u64) -> Result<String> {
    use p2pagents::ops::{MonitorStore, report};
    let monitors = MonitorStore::load(&profile.monitors_path())?.monitors;
    let since_ms = p2pagents::runs::unix_now().saturating_sub(hours * 3600) * 1000;
    let audit: Vec<_> = AuditLog::read(&profile.audit_path(), usize::MAX)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.ts_ms >= since_ms)
        .collect();
    let runs = p2pagents::runs::RunRecord::list(&profile.runs_dir()).unwrap_or_default();
    Ok(report(
        &profile.name,
        hours,
        &monitors,
        &profile.shares()?,
        &profile.ops_store(),
        &audit,
        &runs,
    ))
}

fn run_kb(command: KbCommand) -> Result<()> {
    match command {
        KbCommand::List { profile } => {
            let profile = Profile::open(&profile.name)?;
            let topics = profile.knowledge().list();
            if topics.is_empty() {
                println!(
                    "profile '{}' has no knowledge yet: run the discover playbook, or kb add",
                    profile.name
                );
            }
            for t in topics {
                let kind = match &t.tsg {
                    Some(g) => format!(
                        "TSG ({}, fix: {}{})",
                        g.status,
                        g.fix,
                        g.monitor
                            .as_ref()
                            .map(|m| format!(", monitor {m}"))
                            .unwrap_or_default()
                    ),
                    None => format!("{} chars", t.chars),
                };
                println!("  {:<28} {kind}", t.name);
            }
        }
        KbCommand::Show { topic, profile } => {
            print!(
                "{}",
                Profile::open(&profile.name)?.knowledge().read(&topic)?
            )
        }
        KbCommand::Add {
            topic,
            file,
            profile,
        } => {
            let text = if file.as_os_str() == "-" {
                let mut text = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
                text
            } else {
                std::fs::read_to_string(&file)
                    .with_context(|| format!("reading {}", file.display()))?
            };
            let profile = Profile::open(&profile.name)?;
            profile.knowledge().write(&topic, &text, "added by hand")?;
            println!("profile '{}': saved topic {topic}", profile.name);
        }
        KbCommand::Remove { topic, profile } => {
            if !Profile::open(&profile.name)?.knowledge().remove(&topic)? {
                bail!("there is no topic '{topic}'");
            }
        }
        KbCommand::Search { words, profile } => {
            for topic in Profile::open(&profile.name)?
                .knowledge()
                .search(&words.join(" "))
            {
                println!("  {topic}");
            }
        }
        KbCommand::Learn { hours, profile } => {
            let profile = Profile::open(&profile.name)?;
            let report = ops_report(&profile, hours)?;
            profile.knowledge().write(
                "ops-summary",
                &report,
                &format!("ops report of the last {hours} hours"),
            )?;
            println!(
                "profile '{}': wrote topic ops-summary ({} chars); analyze it with the learn playbook",
                profile.name,
                report.chars().count()
            );
        }
    }
    Ok(())
}

/// Signs `profile` in to GitHub with GitHub CLI account `user@host`; returns the
/// verified login.
fn github_login(profile: &mut Profile, user: &str, host: &str) -> Result<String> {
    profile.github_login(user, host)
}

fn run_github(command: GithubCommand) -> Result<()> {
    match command {
        GithubCommand::Login {
            gh_user,
            host,
            profile,
        } => {
            let mut profile = Profile::open(&profile.name)?;
            let login = github_login(&mut profile, &gh_user, &host)?;
            println!(
                "profile '{}': GitHub repositories as {login}@{host}",
                profile.name
            );
        }
        GithubCommand::Status { profile } => {
            let profile = Profile::open(&profile.name)?;
            match profile.github() {
                Ok(c) => println!(
                    "profile '{}': GitHub as {}@{}",
                    profile.name, c.user, c.host
                ),
                Err(e) => println!("{e:#}"),
            }
            println!("GitHub CLI accounts on this machine:");
            for (login, host, active) in p2pagents::github::gh_accounts() {
                println!("  {login}@{host}{}", if active { " (active)" } else { "" });
            }
        }
        GithubCommand::Logout { profile } => {
            let mut profile = Profile::open(&profile.name)?;
            profile.config.github = None;
            profile.save()?;
        }
    }
    Ok(())
}

async fn run_repo(command: RepoCommand) -> Result<()> {
    use p2pagents::github::{RepoStore, link, task_branches, unlink};
    match command {
        RepoCommand::Link {
            repo,
            name,
            access,
            checks,
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            let checks = p2pagents::shares::parse_checks(&checks)?;
            let (repo, _) = link(&profile, &repo, name.as_deref(), access, checks).await?;
            println!(
                "profile '{}': linked {} ({}) as folder '{}' at {} (at most {access}, a worktree per task)",
                profile.name,
                repo.slug,
                repo.branch,
                repo.name,
                repo.path.display()
            );
            println!(
                "grant it to a peer: p2pagents peers allow <peer> --profile {} --delegate --access {access} --share data,{}",
                profile.name, repo.name
            );
        }
        RepoCommand::List { profile } => {
            for r in RepoStore::load(&Profile::open(&profile.name)?.repos_path())?.repos {
                println!(
                    "  {:<14} {}/{} ({})  {}",
                    r.name,
                    r.host,
                    r.slug,
                    r.branch,
                    r.path.display()
                );
            }
        }
        RepoCommand::Sync { name, profile } => {
            let profile = Profile::open(&profile.name)?;
            let repo = RepoStore::load(&profile.repos_path())?.get(&name)?.clone();
            let head = profile.github()?.sync(&repo).await?;
            println!("{name}: {} at {head}", repo.branch);
        }
        RepoCommand::Branches { name, profile } => {
            let profile = Profile::open(&profile.name)?;
            let repo = RepoStore::load(&profile.repos_path())?.get(&name)?.clone();
            for b in task_branches(&repo).await? {
                println!(
                    "  {:<36} {}  {}",
                    b.name,
                    p2pagents::runs::utc(b.unix),
                    b.subject
                );
            }
        }
        RepoCommand::Pr {
            name,
            branch,
            title,
            body,
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            let repo = RepoStore::load(&profile.repos_path())?.get(&name)?.clone();
            let creds = profile.github()?;
            let title = match title {
                Some(t) => t,
                None => task_branches(&repo)
                    .await?
                    .into_iter()
                    .find(|b| b.name == branch)
                    .map(|b| b.subject)
                    .with_context(|| {
                        format!("{name} has no branch {branch} (see repo branches)")
                    })?,
            };
            creds.push(&repo, &branch).await?;
            println!("{}", creds.create_pr(&repo, &branch, &title, &body).await?);
        }
        RepoCommand::Unlink {
            name,
            delete,
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            unlink(&profile, &name, delete)?;
            println!("profile '{}': unlinked {name}", profile.name);
        }
    }
    Ok(())
}

fn read_input(file: &std::path::Path) -> Result<String> {
    if file.as_os_str() == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        Ok(text)
    } else {
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))
    }
}

/// The built-in checks `part` names: a check (full or short name), a group or all.
fn spec_checks(part: &str, dir: &std::path::Path) -> Result<Vec<&'static str>> {
    use p2pagents::spec::BUILT_IN_CHECKS;
    let group = |prefix: &str| -> Vec<&'static str> {
        BUILT_IN_CHECKS
            .iter()
            .copied()
            .filter(|n| n.starts_with(prefix))
            .collect()
    };
    Ok(match part {
        "all" => {
            let mut names = group("spec:");
            for (folder, prefix) in [("design", "design:"), ("review", "review:")] {
                if dir.join(folder).is_dir() {
                    names.extend(group(prefix));
                } else {
                    println!("{folder}/: not started (p2pagents design init writes a starter)");
                }
            }
            // A rehearsal is optional: the rehearse playbook writes it.
            if !dir.join("review").join("rehearsal.md").exists() {
                names.retain(|n| *n != "review:rehearsal");
            }
            names
        }
        "design" | "review" => group(&format!("{part}:")),
        name if BUILT_IN_CHECKS.contains(&name) => {
            vec![
                *BUILT_IN_CHECKS
                    .iter()
                    .find(|n| **n == name)
                    .expect("listed"),
            ]
        }
        short => {
            let found: Vec<&'static str> = BUILT_IN_CHECKS
                .iter()
                .copied()
                .filter(|n| n.split_once(':').is_some_and(|(_, s)| s == short))
                .collect();
            if found.len() != 1 {
                bail!(
                    "there is no check '{short}' (checks: {}; groups: spec, design, review, all)",
                    BUILT_IN_CHECKS.join(", ")
                );
            }
            found
        }
    })
}

fn run_spec(command: SpecCommand) -> Result<()> {
    use p2pagents::spec;
    match command {
        SpecCommand::Check { part, dir } => {
            let names = spec_checks(part.trim(), &dir)?;
            let mut failed = Vec::new();
            for name in names {
                let report = spec::run(name, &dir);
                println!(
                    "{name}: {}",
                    if report.passed() { "passed" } else { "FAILED" }
                );
                for note in &report.notes {
                    println!("  ok: {note}");
                }
                for problem in &report.problems {
                    println!("  - {problem}");
                }
                if !report.passed() {
                    failed.push(name);
                }
            }
            if !failed.is_empty() {
                bail!("{} failed", failed.join(", "));
            }
        }
        SpecCommand::Init { title, dir, force } => {
            for path in spec::init(&dir, &title, force)? {
                println!("created {path}");
            }
            println!(
                "Edit them, then: p2pagents spec check --dir {}\n\
                 Or let peers write them: pick the 'spec' playbook in Plan mode.",
                dir.display()
            );
        }
    }
    Ok(())
}

fn run_design(command: DesignCommand) -> Result<()> {
    use p2pagents::{design, preview};
    match command {
        DesignCommand::Init { title, dir, force } => {
            for path in design::init(&dir, &title, force)? {
                println!("created {path}");
            }
            println!(
                "Edit them, then: p2pagents spec check design --dir {0} (and review)\n\
                 See the review pack: p2pagents design render --dir {0}\n\
                 Or let peers write them: pick the 'design' playbook in Plan mode.",
                dir.display()
            );
        }
        DesignCommand::Render { dir, out } => {
            let out = out.unwrap_or_else(|| dir.join("review").join("html"));
            let sources = preview::sources_from_dir(&dir);
            if preview::label(&sources) != Some("review pack") {
                bail!(
                    "{} has no review/ decks or design/ documents to render (p2pagents design init writes a starter)",
                    dir.display()
                );
            }
            let files = preview::site(&sources);
            for (path, content) in &files {
                let target = out.join(path);
                std::fs::create_dir_all(target.parent().expect("has a parent"))?;
                std::fs::write(&target, content)
                    .with_context(|| format!("writing {}", target.display()))?;
            }
            println!(
                "wrote {} files; open {}",
                files.len(),
                out.join("index.html").display()
            );
        }
    }
    Ok(())
}

fn run_skill(command: SkillCommand) -> Result<()> {
    match command {
        SkillCommand::List { profile } => {
            for s in Profile::open(&profile.name)?.skills().list() {
                println!("  {:<22} {:<9} {}", s.name, s.source, s.description);
            }
        }
        SkillCommand::Show { name, profile } => {
            print!("{}", Profile::open(&profile.name)?.skills().text(&name)?)
        }
        SkillCommand::Add {
            name,
            file,
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            profile.skills().add(&name, &read_input(&file)?)?;
            println!("profile '{}': saved skill {name}", profile.name);
        }
        SkillCommand::Remove { name, profile } => {
            if !Profile::open(&profile.name)?.skills().remove(&name)? {
                bail!("profile has no skill '{name}' of its own");
            }
        }
        SkillCommand::Import {
            path,
            rename,
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            let (found, text) = p2pagents::skills::read_skill_file(&path)?;
            let name = rename.unwrap_or(found);
            let skills = profile.skills();
            let replaces = skills.list().into_iter().find(|s| s.name == name);
            skills.add(&name, &text)?;
            println!(
                "profile '{}': imported skill {name}{}. Use it with skill={name} in a plan step.",
                profile.name,
                match replaces.map(|s| s.source) {
                    Some("built-in") => " (replaces the built-in one)",
                    Some("profile") => " (replaces the profile's own)",
                    Some(_) => " (used instead of the plugin's)",
                    None => "",
                }
            );
        }
    }
    Ok(())
}

fn run_playbook(command: PlaybookCommand) -> Result<()> {
    use p2pagents::playbooks;
    match command {
        PlaybookCommand::List => {
            for (name, description) in playbooks::list() {
                let vars: Vec<String> = playbooks::variables(name)?
                    .into_iter()
                    .map(|(v, d)| match d {
                        Some(d) => format!("{v}={d}"),
                        None => format!("{v} (required)"),
                    })
                    .collect();
                println!(
                    "  {name:<15} {description}\n                  --set {}",
                    vars.join(", ")
                );
            }
        }
        PlaybookCommand::Explain { name } => {
            let description = playbooks::list()
                .into_iter()
                .find(|(n, _)| *n == name)
                .map(|(_, d)| d)
                .unwrap_or_default();
            let o = playbooks::outline(&name)?;
            println!("{name}: {description}\n\nWhat it does, step by step:");
            for (i, step) in o.steps.iter().enumerate() {
                println!("  {}. {}", i + 1, step.replace("**", ""));
            }
            println!("\nWhat it proves (only with passing checks or a sign-off):");
            for (id, claim) in &o.claims {
                println!("  - {id}: {claim}");
            }
            println!("\nWhat you need:");
            for need in &o.needs {
                println!("  - {need}");
            }
            println!(
                "\nFill it in: p2pagents playbook show {name} --set peer=<peer> ... > {name}.plan, then /plan {name}.plan"
            );
        }
        PlaybookCommand::Show { name, set } => {
            print!("{}", playbooks::render(&name, &set_vars(&set)?)?);
        }
    }
    Ok(())
}

/// `--set NAME=VALUE` playbook values.
fn set_vars(set: &[String]) -> Result<std::collections::BTreeMap<String, String>> {
    let mut vars = std::collections::BTreeMap::new();
    for pair in set {
        let (k, v) = pair
            .split_once('=')
            .with_context(|| format!("--set {pair:?} must look like NAME=VALUE"))?;
        vars.insert(k.trim().to_owned(), v.to_owned());
    }
    Ok(vars)
}

fn run_schedule(command: ScheduleCommand) -> Result<()> {
    use p2pagents::schedule::{Schedule, ScheduleStore};
    let when = |s: &Schedule| {
        s.next_run()
            .map(|t| t.format("%a %Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "paused".into())
    };
    match command {
        ScheduleCommand::Add {
            name,
            when: at,
            plan,
            playbook,
            set,
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            let (text, source) = match (plan, playbook) {
                (Some(file), None) => (
                    std::fs::read_to_string(&file)
                        .with_context(|| format!("reading plan file {}", file.display()))?,
                    file.display().to_string(),
                ),
                (None, Some(book)) => (
                    p2pagents::playbooks::render(&book, &set_vars(&set)?)?,
                    format!("playbook {book}"),
                ),
                _ => bail!("give the plan to run: --plan FILE or --playbook NAME (with --set)"),
            };
            let schedule = Schedule::new(&name, &at, &text, &source)?;
            let next = when(&schedule);
            ScheduleStore::update(&profile.schedules_path(), |store| {
                store.add(schedule);
                Ok(())
            })?;
            println!(
                "profile '{}': scheduled '{name}' ({at}; next run {next}). It runs while this profile's agent runs; each run is an evidence run.",
                profile.name
            );
        }
        ScheduleCommand::List { profile } => {
            let profile = Profile::open(&profile.name)?;
            for s in ScheduleStore::load(&profile.schedules_path())?.schedules {
                println!(
                    "  {:<14} {:<20} next: {:<22} {}{}",
                    s.name,
                    s.when,
                    when(&s),
                    s.source,
                    s.last_result
                        .as_deref()
                        .map(|r| format!("\n  {:<14} last: {r}", ""))
                        .unwrap_or_default()
                );
            }
        }
        ScheduleCommand::Remove { name, profile } => {
            let profile = Profile::open(&profile.name)?;
            ScheduleStore::update(&profile.schedules_path(), |store| {
                if !store.remove(&name) {
                    bail!("profile '{}' has no schedule named '{name}'", profile.name);
                }
                Ok(())
            })?;
            println!("profile '{}': removed schedule '{name}'", profile.name);
        }
        ScheduleCommand::Pause { name, profile } => set_enabled(&name, &profile.name, false)?,
        ScheduleCommand::Resume { name, profile } => set_enabled(&name, &profile.name, true)?,
    }
    Ok(())
}

fn set_enabled(name: &str, profile: &str, enabled: bool) -> Result<()> {
    use p2pagents::schedule::ScheduleStore;
    let profile = Profile::open(profile)?;
    ScheduleStore::update(&profile.schedules_path(), |store| {
        store
            .get_mut(name)
            .with_context(|| format!("profile '{}' has no schedule named '{name}'", profile.name))?
            .set_enabled(enabled);
        Ok(())
    })?;
    println!(
        "profile '{}': schedule '{name}' {}",
        profile.name,
        if enabled {
            "resumed (its time counts from now)"
        } else {
            "paused"
        }
    );
    Ok(())
}

fn run_monitor(command: MonitorCommand) -> Result<()> {
    use p2pagents::ops::{Monitor, MonitorStore};
    match command {
        MonitorCommand::Add {
            name,
            share,
            check,
            every,
            fail_after,
            slo,
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            let mut store = MonitorStore::load(&profile.monitors_path())?;
            store.set(
                Monitor {
                    name: name.clone(),
                    share,
                    check,
                    every_secs: every,
                    fail_after,
                    slo,
                },
                &profile.shares()?,
            )?;
            store.save(&profile.monitors_path())?;
            println!(
                "profile '{}': monitor {name} every {every}s (SLO {slo}%); it runs while the profile's agent runs",
                profile.name
            );
        }
        MonitorCommand::List { profile } => {
            for m in MonitorStore::load(&Profile::open(&profile.name)?.monitors_path())?.monitors {
                println!(
                    "  {:<12} {}:{} every {}s, incident after {} failures, SLO {}%",
                    m.name, m.share, m.check, m.every_secs, m.fail_after, m.slo
                );
            }
        }
        MonitorCommand::Remove { name, profile } => {
            let profile = Profile::open(&profile.name)?;
            let mut store = MonitorStore::load(&profile.monitors_path())?;
            if !store.remove(&name) {
                bail!("there is no monitor '{name}'");
            }
            store.save(&profile.monitors_path())?;
        }
    }
    Ok(())
}

fn run_ops(command: OpsCommand) -> Result<()> {
    match command {
        OpsCommand::Report { hours, profile } => {
            print!("{}", ops_report(&Profile::open(&profile.name)?, hours)?)
        }
        OpsCommand::Incidents { hours, profile } => {
            let since = p2pagents::runs::unix_now().saturating_sub(hours * 3600);
            for e in Profile::open(&profile.name)?.ops_store().incidents(since) {
                println!(
                    "{}  {:<10} {:<10} {}",
                    p2pagents::runs::utc(e.ts),
                    e.monitor,
                    format!("{:?}", e.kind).to_lowercase(),
                    e.text.lines().next().unwrap_or_default()
                );
            }
        }
    }
    Ok(())
}

fn run_runs(command: RunsCommand) -> Result<()> {
    use p2pagents::runs::{RunRecord, utc};
    match command {
        RunsCommand::List { profile } => {
            let profile = Profile::open(&profile.name)?;
            let runs = RunRecord::list(&profile.runs_dir())?;
            if runs.is_empty() {
                println!(
                    "profile '{}' has no evidence runs yet (run a plan)",
                    profile.name
                );
            }
            for run in runs {
                println!(
                    "  {}  {} UTC  {}  {}/{} claims proven, {}/{} steps done",
                    run.id,
                    utc(run.started_unix),
                    if run.passed { "PASS" } else { "FAIL" },
                    run.proven,
                    run.claims,
                    run.steps_done,
                    run.steps
                );
            }
        }
        RunsCommand::Show { id, profile } => {
            let profile = Profile::open(&profile.name)?;
            print!(
                "{}",
                RunRecord::load(&profile.runs_dir(), &id)?.render_markdown()
            );
        }
        RunsCommand::Verify { id, dir, profile } => {
            let dir = match (dir, id, profile) {
                (Some(dir), _, _) => dir,
                (None, Some(id), Some(profile)) => Profile::open(&profile)?.runs_dir().join(id),
                _ => unreachable!("clap requires a run id with --profile, or --dir"),
            };
            let path = dir.join("run.json");
            let run: RunRecord = serde_json::from_str(
                &std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?,
            )
            .with_context(|| format!("parsing {}", path.display()))?;
            let mut failed = false;
            let (run, checks) = run.reverified(Some(&dir));
            for (step, result) in &checks {
                match result {
                    Ok(text) => println!("  ok      {step}: {text}"),
                    Err(e) => {
                        failed = true;
                        println!("  FAILED  {step}: {e:#}");
                    }
                }
            }
            println!("run {}: {}", run.id, run.verdict_line());
            if failed {
                bail!("some evidence doesn't verify");
            }
            println!("all {} signed records verify", checks.len());
            println!("signers (check these keys are the people you expect):");
            for (name, id) in &run.peer_ids {
                println!("  {name:<12} {id}");
            }
        }
    }
    Ok(())
}

fn run_squad(command: SquadCommand) -> Result<()> {
    match command {
        SquadCommand::Create {
            name,
            member,
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            let trust = profile.trust()?;
            if let Some(unknown) = member.iter().find(|m| trust.get_by_name(m).is_none()) {
                bail!(
                    "'{unknown}' is not on profile '{}''s allowlist; add it with: p2pagents peers allow <ticket> --profile {}",
                    profile.name,
                    profile.name
                );
            }
            let mut squads = SquadStore::load(&profile.squads_path())?;
            squads.set(&name, member)?;
            squads.save(&profile.squads_path())?;
            let squad = squads.get(&name).expect("just set");
            println!(
                "profile '{}': squad {name} = {}",
                profile.name,
                squad.members.join(", ")
            );
        }
        SquadCommand::List { profile } => {
            let profile = Profile::open(&profile.name)?;
            for squad in SquadStore::load(&profile.squads_path())?.squads {
                println!("  {:<12} {}", squad.name, squad.members.join(", "));
            }
        }
        SquadCommand::Remove { name, profile } => {
            let profile = Profile::open(&profile.name)?;
            let mut squads = SquadStore::load(&profile.squads_path())?;
            if !squads.remove(&name) {
                bail!("profile '{}' has no squad {name}", profile.name);
            }
            squads.save(&profile.squads_path())?;
            println!("profile '{}': removed squad {name}", profile.name);
        }
    }
    Ok(())
}

fn run_share(command: ShareCommand) -> Result<()> {
    match command {
        ShareCommand::Add {
            name,
            path,
            access,
            checks,
            worktrees,
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            let mut shares = profile.shares()?;
            let checks = p2pagents::shares::parse_checks(&checks)?;
            let share = shares
                .add_with(&name, &path, access, checks, worktrees)?
                .clone();
            shares.save(&profile.shares_path())?;
            println!(
                "profile '{}': sharing {} as '{name}' (at most {access}){}. Grant it to a peer with: p2pagents peers allow <peer> --profile {} --delegate --share data,{name}",
                profile.name,
                share.path.display(),
                share_extras(&share),
                profile.name
            );
        }
        ShareCommand::List { profile } => {
            let profile = Profile::open(&profile.name)?;
            for share in profile.shares()?.shares {
                println!(
                    "  {:<12} at most {:<16} {}{}",
                    share.name,
                    share.max_access.to_string(),
                    share.path.display(),
                    share_extras(&share)
                );
            }
        }
        ShareCommand::Remove { name, profile } => {
            let profile = Profile::open(&profile.name)?;
            let mut shares = profile.shares()?;
            if !shares.remove(&name)? {
                bail!("profile '{}' shares no folder named '{name}'", profile.name);
            }
            shares.save(&profile.shares_path())?;
            println!("profile '{}': stopped sharing '{name}'", profile.name);
        }
    }
    Ok(())
}

fn run_mcp(command: McpCommand) -> Result<()> {
    use p2pagents::mcp::{Fill, McpServer, McpTemplate, parse_config, parse_pairs, server_name};
    use p2pagents::shares::McpOffer;
    let tools_text = |tools: &[String]| {
        if tools.is_empty() {
            "all".to_owned()
        } else {
            tools.join(", ")
        }
    };
    match command {
        McpCommand::Add {
            share,
            name,
            url,
            headers,
            sse,
            env,
            plugin,
            server,
            set,
            tools,
            access,
            description,
            profile,
            command,
        } => {
            let profile = Profile::open(&profile.name)?;
            let offer = McpOffer {
                share: share.clone(),
                name: name.clone(),
                access,
                tools,
                description: description.unwrap_or_default(),
                replace: true,
            };
            let (server, _) = if let Some(plugin) = plugin {
                let store = profile.plugins();
                let p = store.get(&plugin)?;
                let template = p.pick_server(server.as_deref(), &name)?;
                let mut values = parse_pairs(&set, "value")?;
                values_from_env(&p.mcp[&template], &mut values);
                profile.update_shares(|shares| {
                    shares.offer_mcp(&offer, |folder| {
                        store.instantiate(&plugin, &template, folder, &values)
                    })
                })?
            } else {
                let mut server = match (url, command.split_first()) {
                    (Some(url), None) => McpServer::remote(url, sse),
                    (None, Some((cmd, args))) => McpServer::local(cmd.clone(), args.to_vec()),
                    (Some(_), Some(_)) => {
                        bail!("give either --url or a command after --, not both")
                    }
                    (None, None) => bail!(
                        "give the server's command after -- (local), its --url (remote) or --plugin"
                    ),
                };
                server.env = parse_pairs(&env, "variable")?;
                server.headers = parse_pairs(&headers, "header")?;
                profile.update_shares(|shares| shares.offer_mcp(&offer, |_| Ok(server)))?
            };
            println!(
                "profile '{}': tasks in '{share}' with at least {access} access may use MCP server '{name}' ({}); tools: {}. Peers ask for it with mcp={name} in a plan step.",
                profile.name,
                server.describe(),
                tools_text(&server.tools)
            );
        }
        McpCommand::Import {
            file,
            share,
            servers,
            set,
            access,
            replace,
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            let text = read_input(&file)?;
            let config: serde_json::Value =
                serde_json::from_str(text.trim_start_matches('\u{feff}'))
                    .with_context(|| format!("{} isn't valid JSON", file.display()))?;
            let (mut found, notes) = parse_config(&config, &Default::default())?;
            for note in &notes {
                println!("  note: {note}");
            }
            let chosen: Vec<(String, McpTemplate)> = if servers.is_empty() {
                found.into_iter().collect()
            } else {
                let all = found.keys().cloned().collect::<Vec<_>>().join(", ");
                servers
                    .iter()
                    .map(|s| {
                        found.remove(s).map(|t| (s.clone(), t)).with_context(|| {
                            format!("{} has no server '{s}' (it has: {all})", file.display())
                        })
                    })
                    .collect::<Result<_>>()?
            };
            if chosen.is_empty() {
                bail!("{} has no servers to offer", file.display());
            }
            let mut values = parse_pairs(&set, "value")?;
            for (_, template) in &chosen {
                values_from_env(template, &mut values);
            }
            let (offered, _) = profile.update_shares(|shares| {
                let mut offered = Vec::new();
                for (original, template) in &chosen {
                    let offer = McpOffer {
                        share: share.clone(),
                        name: server_name(original),
                        access,
                        tools: Vec::new(),
                        description: String::new(),
                        replace,
                    };
                    let server = shares
                        .offer_mcp(&offer, |folder| {
                            template.instantiate(&Fill {
                                plugin_root: None,
                                data_dir: None,
                                folder,
                                values: &values,
                            })
                        })
                        .with_context(|| format!("server '{original}'"))?;
                    offered.push((offer.name, server));
                }
                Ok(offered)
            })?;
            for (name, server) in offered {
                println!(
                    "profile '{}': '{share}' offers MCP server '{name}' ({}) to tasks with at least {access} access; tools: {}",
                    profile.name,
                    server.describe(),
                    tools_text(&server.tools)
                );
            }
        }
        McpCommand::List { profile } => {
            let profile = Profile::open(&profile.name)?;
            for share in profile.shares()?.shares {
                for (name, server) in &share.mcp {
                    let state = match (server.paused, server.plugin_off) {
                        (_, true) => "  [plugin off]",
                        (true, _) => "  [paused]",
                        _ => "",
                    };
                    println!(
                        "  {:<20} {}+ access, tools: {}  {}{}{state}",
                        format!("{}/{name}", share.name),
                        server.access,
                        tools_text(&server.tools).replace(", ", ","),
                        server.describe(),
                        server
                            .from
                            .as_ref()
                            .map(|f| format!("  (plugin {}: {})", f.plugin, f.server))
                            .unwrap_or_default(),
                    );
                    if !server.description.is_empty() {
                        println!("  {:<20} {}", "", server.description);
                    }
                }
            }
        }
        McpCommand::Pause {
            share,
            name,
            profile,
        } => set_mcp_paused(&share, &name, &profile.name, true)?,
        McpCommand::Resume {
            share,
            name,
            profile,
        } => set_mcp_paused(&share, &name, &profile.name, false)?,
        McpCommand::Remove {
            share,
            name,
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            profile.update_shares(|shares| {
                if !shares.remove_mcp(&share, &name)? {
                    bail!("folder '{share}' offers no MCP server named '{name}'");
                }
                Ok(())
            })?;
            println!(
                "profile '{}': '{share}' no longer offers MCP server '{name}' (a running agent stops offering it within seconds)",
                profile.name
            );
        }
    }
    Ok(())
}

/// Fills values a server template needs, not given with --set, from environment
/// variables of the same name.
fn values_from_env(
    template: &p2pagents::mcp::McpTemplate,
    values: &mut std::collections::BTreeMap<String, String>,
) {
    for input in &template.inputs {
        if !values.contains_key(&input.name)
            && let Ok(value) = std::env::var(&input.name)
            && !value.is_empty()
        {
            println!("  using {} from the environment", input.name);
            values.insert(input.name.clone(), value);
        }
    }
}

fn set_mcp_paused(share: &str, name: &str, profile: &str, paused: bool) -> Result<()> {
    let profile = Profile::open(profile)?;
    profile.update_shares(|shares| {
        shares.edit_mcp(share, name, |server| {
            server.paused = paused;
            Ok(())
        })
    })?;
    println!(
        "profile '{}': MCP server '{name}' in '{share}' is {}",
        profile.name,
        if paused {
            "paused (offered to no task)"
        } else {
            "offered again"
        }
    );
    Ok(())
}

async fn run_plugin(command: PluginCommand) -> Result<()> {
    use p2pagents::plugins::{Inspection, Source, fetch, inspect};
    let print_changes = |lines: &[String]| {
        for line in lines {
            println!("  {line}");
        }
    };
    match command {
        PluginCommand::Add {
            source,
            plugin,
            rename,
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            let store = profile.plugins();
            let creds = profile.github().ok();
            let fetched = fetch(&Source::parse(&source)?, &store.staging(), creds.as_ref()).await?;
            let entry = match inspect(&fetched.root)? {
                Inspection::Plugin(_) if plugin.is_some() => {
                    bail!("{source} is a plugin, not a marketplace: leave out --plugin")
                }
                Inspection::Plugin(_) => None,
                Inspection::Marketplace { name, plugins } => {
                    let Some(pick) = plugin else {
                        println!("{source} is the marketplace '{name}'. Its plugins:");
                        for e in &plugins {
                            println!("  {:<24} {}", e.name, e.description);
                        }
                        bail!("pick one: p2pagents plugin add {source} --plugin NAME");
                    };
                    let entry = plugins
                        .iter()
                        .find(|e| e.name == pick)
                        .with_context(|| format!("marketplace '{name}' has no plugin '{pick}'"))?;
                    Some(
                        fetched
                            .entry(entry, &store.staging(), creds.as_ref())
                            .await?,
                    )
                }
            };
            let installed = store.install(entry.as_ref().unwrap_or(&fetched), rename.as_deref())?;
            print_plugin(&installed, &profile)?;
            println!(
                "\nprofile '{}': imported plugin '{}'. Nothing in it ran.",
                profile.name, installed.name
            );
            if let Some((server, _)) = installed.mcp.iter().next() {
                println!(
                    "Offer its MCP servers in a shared folder: p2pagents mcp add <folder> {server} --plugin {} [--set NAME=VALUE]",
                    installed.name
                );
            }
        }
        PluginCommand::List { profile } => {
            let profile = Profile::open(&profile.name)?;
            let plugins = profile.plugins().list()?;
            if plugins.is_empty() {
                println!(
                    "profile '{}' has no plugins: p2pagents plugin add <source>",
                    profile.name
                );
            }
            for p in plugins {
                println!(
                    "  {:<20} {:<10} {:<4} {} skills, {} MCP servers  {}",
                    p.name,
                    if p.version.is_empty() {
                        "-"
                    } else {
                        &p.version
                    },
                    if p.enabled { "on" } else { "off" },
                    p.skills.len(),
                    p.mcp.len(),
                    p.source
                );
            }
        }
        PluginCommand::Show { name, profile } => {
            let profile = Profile::open(&profile.name)?;
            print_plugin(&profile.plugins().get(&name)?, &profile)?;
        }
        PluginCommand::Update { name, profile } => {
            let profile = Profile::open(&profile.name)?;
            let store = profile.plugins();
            let old = store.get(&name)?;
            let creds = profile.github().ok();
            let fetched = fetch(
                &Source::parse(&old.source)?,
                &store.staging(),
                creds.as_ref(),
            )
            .await?;
            let ((plugin, changes), _) =
                profile.update_shares(|shares| store.update(&name, &fetched, shares))?;
            println!(
                "profile '{}': updated plugin '{}':",
                profile.name, plugin.name
            );
            print_changes(&changes.lines);
        }
        PluginCommand::Enable { name, profile } => set_plugin_enabled(&name, &profile.name, true)?,
        PluginCommand::Disable { name, profile } => {
            set_plugin_enabled(&name, &profile.name, false)?
        }
        PluginCommand::Remove { name, profile } => {
            let profile = Profile::open(&profile.name)?;
            let store = profile.plugins();
            let (changes, _) = profile.update_shares(|shares| store.remove(&name, shares))?;
            println!("profile '{}': removed plugin '{name}'", profile.name);
            print_changes(&changes.lines);
        }
    }
    Ok(())
}

fn set_plugin_enabled(name: &str, profile: &str, enabled: bool) -> Result<()> {
    let profile = Profile::open(profile)?;
    let store = profile.plugins();
    let (changes, _) = profile.update_shares(|shares| store.set_enabled(name, enabled, shares))?;
    println!(
        "profile '{}': plugin '{name}' is {}",
        profile.name,
        if enabled {
            "on"
        } else {
            "off: its skills and MCP servers no longer apply"
        }
    );
    for line in changes.lines {
        println!("  {line}");
    }
    Ok(())
}

/// Prints what plugin `p` contains and where its MCP servers are offered.
fn print_plugin(p: &p2pagents::plugins::Plugin, profile: &Profile) -> Result<()> {
    println!(
        "{} {}({}, {})",
        p.name,
        if p.version.is_empty() {
            String::new()
        } else {
            format!("{} ", p.version)
        },
        p.format,
        if p.enabled { "on" } else { "off" }
    );
    if !p.description.is_empty() {
        println!("  {}", p.description);
    }
    println!(
        "  from {}{}",
        p.source,
        p.commit
            .as_deref()
            .map(|c| format!(" at {}", &c[..c.len().min(12)]))
            .unwrap_or_default()
    );
    if !p.skills.is_empty() {
        println!("  skills (use with skill=NAME in a plan step):");
        for s in &p.skills {
            println!("    {:<24} {}", s.name, s.description);
        }
    }
    if !p.mcp.is_empty() {
        let shares = profile.shares()?;
        println!(
            "  MCP servers (offer one with: mcp add <folder> <name> --plugin {}):",
            p.name
        );
        for (name, t) in &p.mcp {
            println!("    {:<24} {}", name, t.describe());
            if !t.inputs.is_empty() {
                let needs: Vec<String> = t
                    .inputs
                    .iter()
                    .map(|i| {
                        format!(
                            "{}{}{}",
                            i.name,
                            if i.secret { " (secret)" } else { "" },
                            i.default
                                .as_deref()
                                .map(|d| format!(" [default {d}]"))
                                .unwrap_or_default()
                        )
                    })
                    .collect();
                println!("    {:<24} needs: {}", "", needs.join(", "));
            }
            for share in &shares.shares {
                for (offered, def) in &share.mcp {
                    if def
                        .from
                        .as_ref()
                        .is_some_and(|f| f.plugin == p.name && &f.server == name)
                    {
                        println!(
                            "    {:<24} offered in '{}' as {offered}{}",
                            "",
                            share.name,
                            if def.paused { " (paused)" } else { "" }
                        );
                    }
                }
            }
        }
    }
    for note in &p.notes {
        println!("  note: {note}");
    }
    Ok(())
}

fn run_peers(command: PeersCommand) -> Result<()> {
    match command {
        PeersCommand::Id { profile } => {
            let profile = Profile::open(&profile.name)?;
            let key = profile.identity()?;
            let ticket = Ticket {
                name: profile.name.clone(),
                addr: key.public().into(),
            };
            println!(
                "profile '{}'\n  endpoint id: {}",
                profile.name,
                key.public()
            );
            println!("  ticket:      {}", ticket.encode());
            println!(
                "Give the ticket to peers (they run: p2pagents peers allow <ticket>). A running"
            );
            println!("agent prints a fuller ticket with its current addresses (/ticket).");
        }
        PeersCommand::Allow {
            target,
            profile,
            name,
            delegate,
            tasks_per_hour,
            access,
            share,
        } => {
            let profile = Profile::open(&profile.name)?;
            let ticket = Ticket::parse(&target)?;
            let name = name
                .or_else(|| Some(ticket.name.clone()).filter(|n| !n.is_empty()))
                .context("a bare endpoint id has no name; pass --name")?;
            p2pagents::profile::validate_name(&name).context("invalid peer name")?;
            if ticket.addr.id == profile.identity()?.public() {
                bail!("that is this profile's own id");
            }
            let shared = profile.shares()?;
            if let Some(missing) = share.iter().find(|s| shared.get(s).is_none()) {
                bail!(
                    "profile '{}' shares no folder named '{missing}'; add it first: p2pagents share add {missing} <folder> --profile {}",
                    profile.name,
                    profile.name
                );
            }
            let mut trust = profile.trust()?;
            trust.allow(TrustedPeer {
                name: name.clone(),
                id: ticket.addr.id,
                addrs: ticket.addr.addrs,
                can_delegate: delegate,
                tasks_per_hour,
                access,
                shares: share.clone(),
            });
            trust.save(&profile.trust_path())?;
            let rights = if delegate {
                format!(
                    "may delegate up to {tasks_per_hour} tasks/hour with {access} access in: {}",
                    share.join(", ")
                )
            } else {
                "chat only, may not delegate".into()
            };
            println!(
                "profile '{}': allowed {name} ({}), {rights}",
                profile.name,
                ticket.addr.id.fmt_short()
            );
        }
        PeersCommand::List { profile } => {
            let profile = Profile::open(&profile.name)?;
            let trust = profile.trust()?;
            if trust.peers.is_empty() {
                println!("profile '{}' has no allowlisted peers", profile.name);
            }
            for p in trust.peers {
                let rights = if p.can_delegate {
                    format!(
                        "delegate <= {}/h, {}, folders: {}",
                        p.tasks_per_hour,
                        p.access,
                        p.shares.join(",")
                    )
                } else {
                    "no delegation".into()
                };
                println!("  {:<16} {}  {rights}", p.name, p.id);
            }
        }
        PeersCommand::Remove { peer, profile } => {
            let profile = Profile::open(&profile.name)?;
            let mut trust = profile.trust()?;
            if !trust.remove(&peer) {
                bail!("no allowlisted peer {peer} in profile '{}'", profile.name);
            }
            trust.save(&profile.trust_path())?;
            println!("profile '{}': removed {peer}", profile.name);
        }
    }
    Ok(())
}

async fn run_auth(command: AuthCommand) -> Result<()> {
    match command {
        AuthCommand::Login {
            provider,
            profile,
            gh_user,
            host,
            token,
            api_key,
            bridge,
        } => {
            let method = match (gh_user, token, api_key) {
                (Some(user), _, _) => LoginMethod::GhUser { user, host },
                (None, true, _) => LoginMethod::Token { host },
                (None, false, true) => LoginMethod::ApiKey,
                (None, false, false) => LoginMethod::Browser,
            };
            let mut profile = Profile::open(&profile.name)?;
            profile.check_can_sign_in()?;
            let account = ProviderTools::new(&bridge.path())
                .login(&mut profile, provider, method)
                .await?;
            println!(
                "profile '{}': {provider} signed in as {account}",
                profile.name
            );
        }
        AuthCommand::Logout {
            provider,
            profile,
            bridge,
        } => {
            let mut profile = Profile::open(&profile.name)?;
            profile.check_can_sign_in()?;
            ProviderTools::new(&bridge.path()).logout(&mut profile, provider)?;
            println!("profile '{}': {provider} signed out", profile.name);
        }
        AuthCommand::Status { profile, bridge } => {
            let names = match profile {
                Some(name) => vec![name],
                None => Profile::list()?,
            };
            if names.is_empty() {
                println!(
                    "no profiles yet; create one with: p2pagents auth login copilot --profile <name> --gh-user <login>"
                );
            }
            let tools = ProviderTools::new(&bridge.path());
            for name in names {
                let profile = Profile::open(&name)?;
                println!("profile '{name}' ({})", profile.dir.display());
                if let Some(owner) = &profile.config.owner {
                    println!(
                        "  digital worker of '{owner}': runs on its accounts (see profile '{owner}'){}",
                        profile
                            .config
                            .role
                            .as_deref()
                            .map(|r| format!("; role: {r}"))
                            .unwrap_or_default()
                    );
                    continue;
                }
                for provider in [Provider::Copilot, Provider::Claude, Provider::Codex] {
                    let state = if profile.is_configured(provider) {
                        match tools.check(&profile, provider).await {
                            Ok(account) => format!("signed in: {account}"),
                            Err(e) => format!("ERROR: {e:#}"),
                        }
                    } else {
                        format!("not signed in  ({})", profile.login_hint(provider))
                    };
                    println!("  {provider:<8} {state}");
                }
            }
        }
    }
    Ok(())
}

async fn run_agent(args: RunArgs) -> Result<()> {
    let Some(name) = args.name.clone() else {
        use clap::CommandFactory;
        Cli::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "--name <NAME> is required to run an agent",
            )
            .exit();
    };
    let profile = Profile::open(args.profile.as_deref().unwrap_or(&name))?;
    let executor: Option<Arc<SdkExecutor>> = if args.providers.is_empty() {
        None
    } else {
        Some(
            SdkExecutor::start(ExecutorConfig {
                providers: args.providers.clone(),
                bridge_script: args.bridge.path(),
                profile: profile.clone(),
            })
            .await?,
        )
    };
    let local = TaskContext::new(args.access, std::path::absolute(&args.workspace)?);
    let shares = profile.shares()?;

    let (agent, mut events) = {
        if args.serve && executor.is_none() {
            bail!("--serve requires --providers");
        }
        if args.network == Network::Local && args.listen.is_none() {
            bail!("--network local needs --listen <IP:PORT> (a concrete IP peers can reach)");
        }
        let mut config = AgentConfig::new(name, profile.identity()?);
        config.network = args.network;
        config.bind = args.listen;
        config.trust = profile.trust()?;
        config.owner = profile.config.owner.clone();
        config.role = profile.config.role.clone();
        config.worktrees_dir = profile.worktrees_dir();
        // Known for granting folders to peers; they're offered only while serving.
        config.shares = shares.clone();
        if args.serve {
            let data = profile.data_folder();
            std::fs::create_dir_all(&data)
                .with_context(|| format!("creating the shared data folder {}", data.display()))?;
            config.executor = executor.clone().map(|e| e as Arc<dyn Executor>);
            config.audit = Some(Arc::new(AuditLog::new(profile.audit_path())));
        }
        Agent::bind(config).await?
    };
    agent.watch_shares(
        profile.shares_path(),
        profile.data_folder(),
        Duration::from_secs(2),
    );
    println!(
        "{} [profile '{}'] endpoint id {}",
        agent.name(),
        profile.name,
        agent.id()
    );
    if let Some(owner) = &profile.config.owner {
        println!(
            "digital worker of '{owner}' (runs on its accounts){}",
            profile
                .config
                .role
                .as_deref()
                .map(|r| format!("; role: {r}"))
                .unwrap_or_default()
        );
    }
    if args.network == Network::Internet && !agent.online(Duration::from_secs(15)).await {
        eprintln!("warning: no relay reachable yet; peers behind NAT may not reach this agent");
    }
    let ticket = Ticket {
        name: profile.name.clone(),
        addr: agent.addr(),
    };
    println!("ticket: {}", ticket.encode());
    let allowed = profile.trust()?.peers.len();
    if allowed == 0 {
        println!(
            "no allowlisted peers yet: p2pagents peers allow <their-ticket> --profile {}",
            profile.name
        );
    } else {
        println!("{allowed} allowlisted peer(s); connecting automatically");
    }
    if let Some(executor) = &executor {
        println!(
            "providers: {:?} (your tasks: {} in {})",
            executor.providers(),
            args.access,
            local.workspace.display()
        );
        for (provider, account) in executor.accounts() {
            println!("  {provider}: {account} [profile '{}']", profile.name);
        }
        if args.serve {
            println!("folders shared with peers (each peer sees only those you granted it):");
            for share in &shares.shares {
                println!(
                    "  {:<12} at most {:<16} {}{}",
                    share.name,
                    share.max_access.to_string(),
                    share.path.display(),
                    share_extras(share)
                );
            }
        }
    }

    start_monitors(
        &profile,
        &agent,
        executor.clone().map(|e| e as Arc<dyn Executor>),
    )?;

    for peer in &args.peers {
        let result = match Ticket::parse(peer) {
            Ok(ticket) => agent.connect(ticket.addr).await,
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            eprintln!("{e:#}");
        }
    }
    agent.keep_connected(Duration::from_secs(30));

    if args.ui {
        let info = p2pagents::ui::UiInfo {
            profile: profile.clone(),
            network: args.network,
            serving: args.serve,
            access: args.access,
            workspace: local.workspace.clone(),
            providers: executor.as_ref().map(|e| e.providers()).unwrap_or_default(),
            accounts: executor
                .as_ref()
                .map(|e| e.accounts().to_vec())
                .unwrap_or_default(),
        };
        let local = executor.clone().map(|e| e as Arc<dyn Executor>);
        let bind = SocketAddr::from(([127, 0, 0, 1], args.ui_port));
        let server = p2pagents::ui::start(agent.clone(), events, info, local, bind).await?;
        println!("web UI: {}", server.url);
        println!("(the link includes a private access token; press Ctrl+C to stop the agent)");
        if !args.no_open {
            open_browser(&server.url);
        }
        tokio::signal::ctrl_c().await?;
        agent.shutdown().await;
        return Ok(());
    }
    start_schedules(&profile, &agent)?;

    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                Event::PeerConnected { name, id } => println!("+ {name} ({})", id.fmt_short()),
                Event::PeerDisconnected { name, id } => println!("- {name} ({})", id.fmt_short()),
                Event::PeerRejected { id } => {
                    println!("! refused connection from {id} (not on the allowlist)")
                }
                Event::Chat { from, text } => println!("<{from}> {text}"),
                Event::TaskReceived {
                    from,
                    task_id,
                    provider,
                } => {
                    println!("> task #{task_id} from {from} ({provider}) started")
                }
                Event::TaskCompleted {
                    from,
                    task_id,
                    outcome,
                } => {
                    let outcome = match outcome {
                        Outcome::Done => "done",
                        Outcome::Cancelled => "cancelled",
                        _ => "failed",
                    };
                    println!("> task #{task_id} from {from} {outcome}")
                }
                Event::TaskRefused {
                    from,
                    task_id,
                    reason,
                } => println!("> task #{task_id} from {from} refused: {reason}"),
                Event::ApprovalRequested { id, from, title } => println!(
                    "? {from} asks you to approve: {title}\n  see it with /approvals, then /approve {id} [note] or /reject {id} <why>"
                ),
                Event::ApprovalClosed { .. } => {}
            }
        }
    });

    let tasks = RunningTasks::default();
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = stdin.next_line().await? {
        let line = line.trim();
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        match command {
            "" => {}
            "/quit" => break,
            "/help" => println!("{COMMANDS}"),
            "/tasks" => {
                let running = tasks.list();
                if running.is_empty() {
                    println!("no running tasks");
                }
                for (n, label) in running {
                    println!("  {n}: {label}");
                }
            }
            "/cancel" => match rest.trim().parse::<u64>() {
                Ok(n) => match tasks.cancel(n) {
                    Some(label) => println!("=== {label} cancelled (task {n})"),
                    None => eprintln!("no running task {n}; see /tasks"),
                },
                Err(_) => eprintln!("usage: /cancel <n> (see /tasks)"),
            },
            "/ticket" => {
                println!(
                    "{}",
                    Ticket {
                        name: profile.name.clone(),
                        addr: agent.addr()
                    }
                    .encode()
                )
            }
            "/peers" => {
                for peer in agent.peers() {
                    let shares: Vec<String> = peer
                        .shares
                        .iter()
                        .map(|s| {
                            format!(
                                "{} ({}{})",
                                s.name,
                                s.access,
                                if s.mcp.is_empty() {
                                    String::new()
                                } else {
                                    format!("; MCP: {}", s.mcp.join(", "))
                                }
                            )
                        })
                        .collect();
                    println!(
                        "  {} ({}){} serves {:?}{}",
                        peer.name,
                        peer.id.fmt_short(),
                        peer_kind(&peer),
                        peer.capabilities,
                        if shares.is_empty() {
                            String::new()
                        } else {
                            format!(", folders shared with you: {}", shares.join(", "))
                        }
                    );
                }
            }
            "/squads" => {
                let squads = SquadStore::load(&profile.squads_path()).unwrap_or_default();
                if squads.squads.is_empty() {
                    println!(
                        "no squads; create one with: p2pagents squad create <name> --member a,b --profile {}",
                        profile.name
                    );
                }
                let online: Vec<String> = agent.peers().into_iter().map(|p| p.name).collect();
                for squad in squads.squads {
                    let members: Vec<String> = squad
                        .members
                        .iter()
                        .map(|m| {
                            let state = if online.contains(m) {
                                "online"
                            } else {
                                "offline"
                            };
                            format!("{m} ({state})")
                        })
                        .collect();
                    println!("  #{}: {}", squad.name, members.join(", "));
                }
            }
            "/plan" => {
                let file = rest.trim();
                let text = std::fs::read_to_string(file)
                    .with_context(|| format!("reading plan file {file:?}"));
                let kb = profile.knowledge();
                let skills = profile.skills();
                let squads_now = SquadStore::load(&profile.squads_path()).unwrap_or_default();
                let parsed = text.and_then(|text| {
                    let plan = Plan::parse(&text)?;
                    let problems = p2pagents::plan::preflight(
                        &plan,
                        &agent.peers(),
                        &squads_now,
                        &kb,
                        &skills,
                    );
                    for p in &problems {
                        println!(
                            "  {} {}{}",
                            if p.error { "error:" } else { "warning:" },
                            p.step
                                .as_ref()
                                .map(|s| format!("step {s}: "))
                                .unwrap_or_default(),
                            p.message
                        );
                    }
                    if problems.iter().any(|p| p.error) {
                        bail!("the plan was not started: fix the errors above");
                    }
                    let ready = plan.prepare(&kb, &skills)?;
                    Ok((text, plan, ready))
                });
                match parsed {
                    Err(e) => eprintln!("{e:#}"),
                    Ok((text, plan, ready)) => {
                        let squads = SquadStore::load(&profile.squads_path()).unwrap_or_default();
                        let agent = agent.clone();
                        let runs_dir = profile.runs_dir();
                        let n = tasks.spawn(
                            format!("plan {file} ({} steps)", plan.steps.len()),
                            async move {
                                let started = p2pagents::runs::unix_now();
                                let states = run_plan(&agent, &squads, &ready, print_step).await;
                                let mut run = p2pagents::runs::RunRecord::new(
                                    agent.name(),
                                    &agent.id().to_string(),
                                    &text,
                                    &plan,
                                    &states,
                                    started,
                                );
                                run.peer_ids = p2pagents::runs::peer_ids(&agent);
                                println!("=== plan finished: {} ===", run.verdict_line());
                                for claim in run.claim_results() {
                                    println!(
                                        "  {} {}: {}",
                                        claim.verdict.label(),
                                        claim.claim.id,
                                        claim.claim.text
                                    );
                                }
                                match run.save(&runs_dir) {
                                    Ok(report) => println!("  report: {}", report.display()),
                                    Err(e) => eprintln!("could not save the run: {e:#}"),
                                }
                                let saved =
                                    p2pagents::plan::save_knowledge(&kb, &plan, &states, &run.id);
                                if !saved.is_empty() {
                                    println!("  knowledge saved: {}", saved.join(", "));
                                }
                            },
                        );
                        println!("running plan {file} as task {n} (/cancel {n} to stop it)");
                    }
                }
            }
            "/runs" => match p2pagents::runs::RunRecord::list(&profile.runs_dir()) {
                Ok(runs) if runs.is_empty() => println!("no evidence runs yet"),
                Ok(runs) => {
                    for run in runs.iter().take(10) {
                        println!(
                            "  {}  {}  {}/{} claims proven",
                            run.id,
                            if run.passed { "PASS" } else { "FAIL" },
                            run.proven,
                            run.claims
                        );
                    }
                }
                Err(e) => eprintln!("{e:#}"),
            },
            "/approvals" => {
                let open = agent.approvals();
                if open.is_empty() {
                    println!("no approval requests");
                }
                for a in open {
                    println!(
                        "=== #{} from {}: {} ===\n{}\n{}=== /approve {} [note] or /reject {} <why> ===",
                        a.id,
                        a.from,
                        a.title,
                        a.text,
                        match a.preview {
                            Some("prototype") => {
                                "(it comes with a UX prototype: open it from this request in the web UI, --ui)\n"
                            }
                            Some(_) => {
                                "(it comes with a review pack, decks and design documents: open it from this request in the web UI, --ui)\n"
                            }
                            None => "",
                        },
                        a.id,
                        a.id
                    );
                }
            }
            "/approve" | "/reject" => {
                let (n, note) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
                let approved = command == "/approve";
                match n.parse::<u64>() {
                    Err(_) => eprintln!("usage: {command} <n> [note] (see /approvals)"),
                    Ok(_) if !approved && note.trim().is_empty() => {
                        eprintln!("say why: /reject {n} <why>")
                    }
                    Ok(id) => match agent.answer_approval(id, approved, note).await {
                        Ok(()) => println!(
                            "{} request {id}",
                            if approved { "approved" } else { "rejected" }
                        ),
                        Err(e) => eprintln!("{e:#}"),
                    },
                }
            }
            "/ask" => match (parse_task(rest), &executor) {
                (Err(e), _) => eprintln!("{e:#}"),
                (Ok(_), None) => eprintln!("no local providers; start with --providers"),
                (Ok((task, _)), Some(executor)) => {
                    let executor = executor.clone();
                    let label = format!("{} (local)", task.provider);
                    let ctx = local.clone();
                    tasks.spawn(label.clone(), async move {
                        print_result(&label, executor.execute(&task, &ctx).await);
                    });
                }
            },
            "/delegate" => {
                match parse_task(rest).and_then(|(task, peer)| pick_peer(&agent, &task, peer)) {
                    Err(e) => eprintln!("{e:#}"),
                    Ok((task, peer)) => {
                        let agent = agent.clone();
                        let label = format!("{} on {}", task.provider, peer.name);
                        let n = tasks.spawn(label.clone(), async move {
                            print_result(&label, agent.delegate(peer.id, task).await);
                        });
                        println!(
                            "delegating to {} ({}) as task {n} (/cancel {n} to stop it)",
                            peer.name,
                            peer.id.fmt_short()
                        );
                    }
                }
            }
            _ if command.starts_with('/') => eprintln!("unknown command {command}; try /help"),
            _ => {
                agent.broadcast(line);
            }
        }
    }
    agent.shutdown().await;
    Ok(())
}

/// Parses `<provider>[@peer[:folder]] <prompt>`.
fn parse_task(rest: &str) -> Result<(LlmTask, Option<String>)> {
    let (target, prompt) = rest
        .trim()
        .split_once(' ')
        .context("usage: <provider>[@peer[:folder]] <prompt>")?;
    let (provider, peer) = match target.split_once('@') {
        Some((provider, peer)) => (provider, Some(peer)),
        None => (target, None),
    };
    let (peer, share) = match peer.map(|p| p.split_once(':').unwrap_or((p, ""))) {
        Some((peer, share)) => (Some(peer.to_owned()), Some(share).filter(|s| !s.is_empty())),
        None => (None, None),
    };
    let provider = Provider::from_str(provider, true).map_err(|e| anyhow!("{e}"))?;
    let mut task = LlmTask::new(provider, prompt.trim());
    if let Some(share) = share {
        task = task.in_share(share);
    }
    Ok((task, peer))
}

fn pick_peer(
    agent: &Agent,
    task: &LlmTask,
    name: Option<String>,
) -> Result<(LlmTask, p2pagents::PeerInfo)> {
    let peer = match name {
        Some(name) => agent
            .peers()
            .into_iter()
            .find(|p| p.name == name)
            .ok_or_else(|| anyhow!("no peer named {name}"))?,
        None => agent
            .find_peer(task.provider)
            .ok_or_else(|| anyhow!("no peer serves {}", task.provider))?,
    };
    Ok((task.clone(), peer))
}

/// Last task number, and running tasks by number with their label and abort handle.
type TaskTable = (
    u64,
    std::collections::BTreeMap<u64, (String, tokio::task::AbortHandle)>,
);

/// Tasks started from the prompt, numbered so `/cancel <n>` can stop them.
#[derive(Clone, Default)]
struct RunningTasks {
    inner: Arc<std::sync::Mutex<TaskTable>>,
}

impl RunningTasks {
    fn spawn(
        &self,
        label: String,
        task: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> u64 {
        let mut guard = self.inner.lock().unwrap();
        guard.0 += 1;
        let n = guard.0;
        let this = self.clone();
        let handle = tokio::spawn(async move {
            task.await;
            this.inner.lock().unwrap().1.remove(&n);
        });
        guard.1.insert(n, (label, handle.abort_handle()));
        n
    }

    fn list(&self) -> Vec<(u64, String)> {
        let guard = self.inner.lock().unwrap();
        guard
            .1
            .iter()
            .map(|(n, (label, _))| (*n, label.clone()))
            .collect()
    }

    /// Aborts task `n`; dropping a delegation tells the peer to stop it.
    fn cancel(&self, n: u64) -> Option<String> {
        let (label, handle) = self.inner.lock().unwrap().1.remove(&n)?;
        handle.abort();
        Some(label)
    }
}

/// ` [digital worker of alice: role]`-style suffix for a peer.
fn peer_kind(peer: &p2pagents::PeerInfo) -> String {
    match (&peer.owner, &peer.role) {
        (Some(owner), Some(role)) => format!(" [digital worker of {owner}: {role}]"),
        (Some(owner), None) => format!(" [digital worker of {owner}]"),
        (None, Some(role)) => format!(" [{role}]"),
        (None, None) => String::new(),
    }
}

fn print_step(id: &str, state: &StepState) {
    match state {
        StepState::Waiting => println!("  step {id}: waiting"),
        StepState::Running { peer } => println!("  step {id}: running on {peer}"),
        StepState::Done {
            peer,
            output,
            evidence,
            verified,
            ..
        } => {
            println!("=== step {id} on {peer} ===\n{output}\n=== end ===");
            print_evidence(evidence.as_ref(), *verified);
        }
        StepState::Failed {
            peer,
            error,
            evidence,
            verified,
        } => {
            println!(
                "=== step {id}{} failed: {error}",
                peer.as_deref()
                    .map(|p| format!(" on {p}"))
                    .unwrap_or_default()
            );
            print_evidence(evidence.as_ref(), *verified);
        }
        StepState::Skipped { reason } => println!("  step {id}: skipped ({reason})"),
    }
}

fn print_evidence(evidence: Option<&p2pagents::evidence::Evidence>, verified: bool) {
    if let Some(e) = evidence {
        println!(
            "  evidence: signed by {} ({}) {}",
            e.signer,
            e.signer_id.chars().take(10).collect::<String>(),
            if verified {
                "✓ verified"
            } else {
                "✗ NOT valid"
            }
        );
    }
}

fn print_result(label: &str, result: Result<String>) {
    match result {
        Ok(output) => println!("=== {label} ===\n{output}\n=== end ==="),
        Err(e) => eprintln!("=== {label} failed: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn runs_verify_takes_a_folder_alone_or_an_id_with_a_profile() {
        let parse = |args: &[&str]| Cli::try_parse_from([&["p2pagents"], args].concat()).is_ok();
        assert!(parse(&["runs", "verify", "--dir", "some/run"]));
        assert!(parse(&[
            "runs",
            "verify",
            "20261115-1",
            "--profile",
            "boss"
        ]));
        assert!(!parse(&["runs", "verify", "20261115-1"]));
        assert!(!parse(&["runs", "verify"]));
    }

    #[test]
    fn all_checks_include_the_rehearsal_only_once_there_is_one() {
        let dir = tempfile::tempdir().unwrap();
        for folder in ["design", "review"] {
            std::fs::create_dir(dir.path().join(folder)).unwrap();
        }
        let all = spec_checks("all", dir.path()).unwrap();
        assert!(all.contains(&"review:decks") && !all.contains(&"review:rehearsal"));
        std::fs::write(dir.path().join("review/rehearsal.md"), "# Rehearsal\n").unwrap();
        assert!(
            spec_checks("all", dir.path())
                .unwrap()
                .contains(&"review:rehearsal")
        );
        assert_eq!(
            spec_checks("rehearsal", dir.path()).unwrap(),
            ["review:rehearsal"]
        );
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(11_016), "2000-02-29");
        assert_eq!(civil_date(20_772), "2026-11-15");
    }
}
