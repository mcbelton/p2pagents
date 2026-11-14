//! Peer-to-peer agents that talk over TCP using newline-delimited JSON and run
//! or delegate LLM coding tasks with the GitHub Copilot, Claude Agent and Codex SDKs.
//! Each agent uses its own provider instances and accounts (see [`profile`]).

pub mod agent;
pub mod auth;
pub mod llm;
pub mod profile;
pub mod protocol;

pub use agent::{Agent, Event, PeerInfo};
pub use llm::{Access, Executor, ExecutorConfig, LlmTask, Provider, SdkExecutor};
pub use profile::Profile;
pub use protocol::Message;
