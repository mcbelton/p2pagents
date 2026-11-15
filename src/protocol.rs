use anyhow::Result;
use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::evidence::Evidence;
use crate::llm::{LlmTask, Provider};
use crate::shares::ShareInfo;

/// QUIC ALPN identifying this protocol; peers speaking anything else are refused.
pub const ALPN: &[u8] = b"p2pagents/1";

/// Wire messages, framed as one JSON object per line on a QUIC stream. The
/// sender's identity is not in the messages: it is the connection's
/// cryptographically verified endpoint id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello {
        name: String,
        /// The sender's current addresses, shared with other peers by gossip.
        addr: EndpointAddr,
        /// LLM providers this agent runs tasks with on behalf of its peers.
        #[serde(default)]
        capabilities: Vec<Provider>,
        /// Folders the receiving peer may run tasks in (names and access, no paths).
        #[serde(default)]
        shares: Vec<ShareInfo>,
        /// For a digital worker: the profile that owns it (and whose accounts it uses).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<String>,
        /// What this agent is for, as set by its owner.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
    },
    Peers {
        addrs: Vec<EndpointAddr>,
    },
    Chat {
        from: String,
        text: String,
    },
    TaskRequest {
        task_id: u64,
        task: LlmTask,
    },
    TaskResult {
        task_id: u64,
        ok: bool,
        output: String,
        /// What the worker's runtime observed, signed with its key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<Box<Evidence>>,
        /// The task's commit as a patch (folders shared with `--worktrees`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        patch: Option<String>,
    },
    /// Asks a team member to approve a plan step; answered with `TaskResult`
    /// (`ok` = approved) and withdrawn with `TaskCancel`.
    ApprovalRequest {
        task_id: u64,
        title: String,
        text: String,
    },
    /// The requester no longer wants the result of `task_id`; the worker stops it.
    TaskCancel {
        task_id: u64,
    },
    /// The sender's folders the receiver may now use (sent when its grant changes).
    Shares {
        shares: Vec<ShareInfo>,
    },
    /// A message type from a newer version; ignored so mixed versions keep working.
    #[serde(other)]
    Unknown,
}

pub async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, msg: &Message) -> Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_tagged_json() {
        let msg = Message::Chat {
            from: "a".into(),
            text: "hi".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"chat","from":"a","text":"hi"}"#);
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), msg);
    }

    #[test]
    fn task_request_round_trips() {
        let msg = Message::TaskRequest {
            task_id: 7,
            task: LlmTask::new(Provider::Codex, "fix it"),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"task_request","task_id":7,"task":{"provider":"codex","prompt":"fix it"}}"#
        );
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), msg);
    }

    #[test]
    fn task_cancel_round_trips_and_unknown_messages_are_ignored() {
        let msg = Message::TaskCancel { task_id: 3 };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"task_cancel","task_id":3}"#);
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), msg);
        let future = r#"{"type":"task_progress","task_id":3,"text":"half way"}"#;
        assert_eq!(
            serde_json::from_str::<Message>(future).unwrap(),
            Message::Unknown
        );
    }

    #[test]
    fn hello_with_addresses_round_trips() {
        let id = iroh::SecretKey::generate().public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4000".parse().unwrap());
        let msg = Message::Hello {
            name: "a".into(),
            addr,
            capabilities: vec![Provider::Claude],
            shares: Vec::new(),
            owner: Some("alice".into()),
            role: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), msg);
    }
}
