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
    /// Create digital workers: agents with their own peer identity that run on
    /// their owner's accounts.
    #[command(subcommand)]
    Worker(WorkerCommand),
    /// Group peers into squads that plans can assign steps to.
    #[command(subcommand)]
    Squad(SquadCommand),
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
enum ShareCommand {
    /// Share a folder with peers (grant it per peer with `peers allow --share`).
    #[command(after_help = "\
examples:
  p2pagents share add myrepo D:\\src\\myrepo --access workspace-write --profile alice
  p2pagents peers allow <bob-ticket> --profile alice --delegate --access workspace-write --share data,myrepo
Every profile also shares its own .p2pdata folder as 'data' (read-only); peers'
tasks run there unless they name another folder shared with them.")]
    Add {
        /// Name peers use for the folder (e.g. `/delegate copilot@alice:myrepo ...`).
        name: String,
        /// Existing folder to share.
        path: PathBuf,
        /// Most any peer's task may do in this folder.
        #[arg(long, value_enum, default_value_t = Access::ReadOnly)]
        access: Access,
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
        Some(Command::Worker(command)) => run_worker(command),
        Some(Command::Squad(command)) => run_squad(command),
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
            profile,
        } => {
            let profile = Profile::open(&profile.name)?;
            let mut shares = profile.shares()?;
            let share = shares.add(&name, &path, access)?.clone();
            shares.save(&profile.shares_path())?;
            println!(
                "profile '{}': sharing {} as '{name}' (at most {access}). Grant it to a peer with: p2pagents peers allow <peer> --profile {} --delegate --share data,{name}",
                profile.name,
                share.path.display(),
                profile.name
            );
        }
        ShareCommand::List { profile } => {
            let profile = Profile::open(&profile.name)?;
            for share in profile.shares()?.shares {
                println!(
                    "  {:<12} at most {:<16} {}",
                    share.name,
                    share.max_access.to_string(),
                    share.path.display()
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
    let local = TaskContext {
        access: args.access,
        workspace: std::path::absolute(&args.workspace)?,
    };
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
        if args.serve {
            let data = profile.data_folder();
            std::fs::create_dir_all(&data)
                .with_context(|| format!("creating the shared data folder {}", data.display()))?;
            config.executor = executor.clone().map(|e| e as Arc<dyn Executor>);
            config.audit = Some(Arc::new(AuditLog::new(profile.audit_path())));
            config.shares = shares.clone();
        }
        Agent::bind(config).await?
    };
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
                    "  {:<12} at most {:<16} {}",
                    share.name,
                    share.max_access.to_string(),
                    share.path.display()
                );
            }
        }
    }

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
                        .map(|s| format!("{} ({})", s.name, s.access))
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
                let plan = std::fs::read_to_string(file)
                    .with_context(|| format!("reading plan file {file:?}"))
                    .and_then(|text| Plan::parse(&text));
                match plan {
                    Err(e) => eprintln!("{e:#}"),
                    Ok(plan) => {
                        let squads = SquadStore::load(&profile.squads_path()).unwrap_or_default();
                        let agent = agent.clone();
                        let steps = plan.steps.len();
                        let n = tasks.spawn(format!("plan {file} ({steps} steps)"), async move {
                            let states = run_plan(&agent, &squads, &plan, print_step).await;
                            let done = states
                                .values()
                                .filter(|s| matches!(s, StepState::Done { .. }))
                                .count();
                            println!("=== plan finished: {done}/{steps} steps done ===");
                        });
                        println!("running plan {file} as task {n} (/cancel {n} to stop it)");
                    }
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
        StepState::Done { peer, output } => {
            println!("=== step {id} on {peer} ===\n{output}\n=== end ===")
        }
        StepState::Failed { peer, error } => println!(
            "=== step {id}{} failed: {error}",
            peer.as_deref()
                .map(|p| format!(" on {p}"))
                .unwrap_or_default()
        ),
        StepState::Skipped { reason } => println!("  step {id}: skipped ({reason})"),
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
    fn civil_dates() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(11_016), "2000-02-29");
        assert_eq!(civil_date(20_772), "2026-11-15");
    }
}
