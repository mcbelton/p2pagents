use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use p2pagents::auth::{LoginMethod, ProviderTools};
use p2pagents::profile::DEFAULT_GITHUB_HOST;
use p2pagents::{
    Access, Agent, Event, Executor, ExecutorConfig, LlmTask, Profile, Provider, SdkExecutor,
};
use tokio::io::{AsyncBufReadExt, BufReader};

const COMMANDS: &str = "\
commands:
  <text>                                broadcast a chat message
  /ask <provider> <prompt>              run a task locally (provider: copilot|claude|codex)
  /delegate <provider>[@peer] <prompt>  run a task on a peer that serves <provider>
  /peers                                list peers and the providers they serve
  /help                                 show this help
  /quit                                 exit

Each agent uses its own Copilot/Claude/Codex instances and accounts from its profile
(--profile, default: --name). Sign a profile in first:  p2pagents auth --help";

const BRIDGE_DEFAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/bridge/index.mjs");

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
    #[arg(long = "profile")]
    name: String,
}

#[derive(ClapArgs)]
struct BridgeArg {
    /// Node bridge script for the Claude and Codex SDKs.
    #[arg(long, env = "P2PAGENTS_BRIDGE", default_value = BRIDGE_DEFAULT)]
    bridge: PathBuf,
}

#[derive(ClapArgs)]
struct RunArgs {
    /// Agent name shown to peers (required to run an agent).
    #[arg(long)]
    name: Option<String>,
    /// Profile whose provider instances and accounts this agent uses (default: --name).
    #[arg(long)]
    profile: Option<String>,
    /// Address to listen on (use a concrete IP; it is advertised to peers).
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: SocketAddr,
    /// Peer address to connect to on startup (repeatable).
    #[arg(long = "peer")]
    peers: Vec<SocketAddr>,
    /// LLM providers to enable on this agent (comma-separated).
    #[arg(long, value_enum, value_delimiter = ',')]
    providers: Vec<Provider>,
    /// Run tasks delegated by peers with the enabled providers.
    #[arg(long)]
    serve: bool,
    /// Directory LLM agents work in.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// What LLM agents may do in the workspace.
    #[arg(long, value_enum, default_value_t = Access::ReadOnly)]
    access: Access,
    #[command(flatten)]
    bridge: BridgeArg,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Auth(command)) => run_auth(command).await,
        None => run_agent(cli.run).await,
    }
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
            let account = ProviderTools::new(&bridge.bridge)
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
            ProviderTools::new(&bridge.bridge).logout(&mut profile, provider)?;
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
            let tools = ProviderTools::new(&bridge.bridge);
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
                bridge_script: args.bridge.bridge.clone(),
                profile: profile.clone(),
            })
            .await?,
        )
    };

    let (agent, mut events) = match (&executor, args.serve) {
        (Some(executor), true) => {
            Agent::bind_with_executor(name, args.listen, executor.clone()).await?
        }
        (None, true) => bail!("--serve requires --providers"),
        _ => Agent::bind(name, args.listen).await?,
    };
    println!("{} listening on {}", agent.name(), agent.listen_addr());
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

    for addr in args.peers {
        if let Err(e) = agent.connect(addr).await {
            eprintln!("{e:#}");
        }
    }

    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                Event::PeerConnected { name, addr } => println!("+ {name} ({addr})"),
                Event::PeerDisconnected { name, addr } => println!("- {name} ({addr})"),
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
            "/peers" => {
                for peer in agent.peers() {
                    println!(
                        "  {} ({}) serves {:?}",
                        peer.name, peer.addr, peer.capabilities
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
                        print_result(&label, executor.execute(&task).await);
                    });
                }
            },
            "/delegate" => {
                match parse_task(rest).and_then(|(task, peer)| pick_peer(&agent, &task, peer)) {
                    Err(e) => eprintln!("{e:#}"),
                    Ok((task, peer)) => {
                        let agent = agent.clone();
                        println!("delegating to {} ({})", peer.name, peer.addr);
                        tokio::spawn(async move {
                            let label = format!("{} on {}", task.provider, peer.name);
                            print_result(&label, agent.delegate(peer.addr, task).await);
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
    agent.shutdown();
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
