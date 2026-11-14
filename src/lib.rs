//! Peer-to-peer agents that connect over the internet with iroh (QUIC, NAT
//! traversal, relays), authenticate each other by public key against an
//! allowlist, and run or delegate LLM coding tasks with the GitHub Copilot,
//! Claude Agent and Codex SDKs. Each agent uses its own provider instances and
//! accounts (see [`profile`]).

pub mod agent;
pub mod auth;
pub mod llm;
pub mod profile;
pub mod protocol;
pub mod trust;

pub use agent::{Agent, AgentConfig, Event, Network, PeerInfo};
pub use iroh::{EndpointAddr, EndpointId, SecretKey};
pub use llm::{Access, Executor, ExecutorConfig, LlmTask, Provider, SdkExecutor};
pub use profile::Profile;
pub use protocol::Message;
pub use trust::{Ticket, TrustStore, TrustedPeer};
