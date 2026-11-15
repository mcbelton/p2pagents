use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use p2pagents::auth::{LoginMethod, ProviderTools};
use p2pagents::profile::DEFAULT_GITHUB_HOST;
use p2pagents::trust::DEFAULT_TASKS_PER_HOUR;
use p2pagents::{
    Access, Agent, AgentConfig, Event, Executor, ExecutorConfig, LlmTask, Network, Profile,
    Provider, SdkExecutor, Ticket, TrustedPeer,
};
use tokio::io::{AsyncBufReadExt, BufReader};

const COMMANDS: &str = "\
commands:
  <text>                                broadcast a chat message
  /ask <provider> <prompt>              run a task locally (provider: copilot|claude|codex)
  /delegate <provider>[@peer] <prompt>  run a task on a peer that serves <provider>
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
        /// full (also shell commands). Capped by the agent's --access.
        #[arg(long, value_enum, default_value_t = Access::ReadOnly, requires = "delegate")]
        access: Access,
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
    /// Directory LLM agents work in.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// Most any task on this agent may do: read-only, workspace-write (edit files) or
    /// full (also shell commands). Peer tasks also get no more than the peer's grant.
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
        None => run_agent(cli.run).await,
    }
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
            let mut trust = profile.trust()?;
            trust.allow(TrustedPeer {
                name: name.clone(),
                id: ticket.addr.id,
                addrs: ticket.addr.addrs,
                can_delegate: delegate,
                tasks_per_hour,
                access,
            });
            trust.save(&profile.trust_path())?;
            let rights = if delegate {
                format!("may delegate up to {tasks_per_hour} tasks/hour with {access} access")
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
                    format!("delegate <= {}/h, {}", p.tasks_per_hour, p.access)
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
                workspace: args.workspace.clone(),
                access: args.access,
                bridge_script: args.bridge.path(),
                profile: profile.clone(),
            })
            .await?,
        )
    };

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
        if args.serve {
            config.executor = executor.clone().map(|e| e as Arc<dyn Executor>);
        }
        Agent::bind(config).await?
    };
    println!(
        "{} [profile '{}'] endpoint id {}",
        agent.name(),
        profile.name,
        agent.id()
    );
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
        let served = if args.serve { ", served to peers" } else { "" };
        println!(
            "providers: {:?} ({:?} in {}{served})",
            executor.providers(),
            args.access,
            args.workspace.display()
        );
        for (provider, account) in executor.accounts() {
            println!("  {provider}: {account} [profile '{}']", profile.name);
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
            workspace: std::path::absolute(&args.workspace).unwrap_or(args.workspace.clone()),
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
                Event::TaskCompleted { from, task_id, ok } => {
                    println!(
                        "> task #{task_id} from {from} {}",
                        if ok { "done" } else { "failed" }
                    )
                }
            }
        }
    });

    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = stdin.next_line().await? {
        let line = line.trim();
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        match command {
            "" => {}
            "/quit" => break,
            "/help" => println!("{COMMANDS}"),
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
                    println!(
                        "  {} ({}) serves {:?}",
                        peer.name,
                        peer.id.fmt_short(),
                        peer.capabilities
                    );
                }
            }
            "/ask" => match (parse_task(rest), &executor) {
                (Err(e), _) => eprintln!("{e:#}"),
                (Ok(_), None) => eprintln!("no local providers; start with --providers"),
                (Ok((task, _)), Some(executor)) => {
                    let executor = executor.clone();
                    tokio::spawn(async move {
                        let label = format!("{} (local)", task.provider);
                        print_result(&label, executor.execute(&task, Access::Full).await);
                    });
                }
            },
            "/delegate" => {
                match parse_task(rest).and_then(|(task, peer)| pick_peer(&agent, &task, peer)) {
                    Err(e) => eprintln!("{e:#}"),
                    Ok((task, peer)) => {
                        let agent = agent.clone();
                        println!("delegating to {} ({})", peer.name, peer.id.fmt_short());
                        tokio::spawn(async move {
                            let label = format!("{} on {}", task.provider, peer.name);
                            print_result(&label, agent.delegate(peer.id, task).await);
                        });
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

/// Parses `<provider>[@peer] <prompt>`.
fn parse_task(rest: &str) -> Result<(LlmTask, Option<String>)> {
    let (target, prompt) = rest
        .trim()
        .split_once(' ')
        .context("usage: <provider>[@peer] <prompt>")?;
    let (provider, peer) = match target.split_once('@') {
        Some((provider, peer)) => (provider, Some(peer.to_owned())),
        None => (target, None),
    };
    let provider = Provider::from_str(provider, true).map_err(|e| anyhow!("{e}"))?;
    Ok((LlmTask::new(provider, prompt.trim()), peer))
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
}
