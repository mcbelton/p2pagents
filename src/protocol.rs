use std::net::SocketAddr;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::llm::{LlmTask, Provider};

/// Wire messages, framed as one JSON object per line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello {
        name: String,
        listen_addr: SocketAddr,
        /// LLM providers this agent runs tasks with on behalf of its peers.
        #[serde(default)]
        capabilities: Vec<Provider>,
    },
    Peers {
        addrs: Vec<SocketAddr>,
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
    fn hello_without_capabilities_is_accepted() {
        let json = r#"{"type":"hello","name":"old","listen_addr":"127.0.0.1:1"}"#;
        let Message::Hello { capabilities, .. } = serde_json::from_str(json).unwrap() else {
            panic!("not a hello");
        };
        assert!(capabilities.is_empty());
    }
}
