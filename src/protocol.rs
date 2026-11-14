use anyhow::Result;
use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::llm::{LlmTask, Provider};

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
    },
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
    fn hello_with_addresses_round_trips() {
        let id = iroh::SecretKey::generate().public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4000".parse().unwrap());
        let msg = Message::Hello {
            name: "a".into(),
            addr,
            capabilities: vec![Provider::Claude],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), msg);
    }
}
