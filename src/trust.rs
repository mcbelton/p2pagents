//! Who an agent will talk to. Every peer is identified by its public key
//! (iroh `EndpointId`, authenticated by the QUIC/TLS handshake); connections from
//! keys that are not on the profile's allowlist are dropped, and only peers marked
//! `can_delegate` may hand this agent LLM tasks.

use std::collections::BTreeSet;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use iroh::{EndpointAddr, EndpointId, TransportAddr};
use serde::{Deserialize, Serialize};

pub const DEFAULT_TASKS_PER_HOUR: u32 = 20;

fn default_tasks_per_hour() -> u32 {
    DEFAULT_TASKS_PER_HOUR
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedPeer {
    /// Local alias for the peer (used in `/delegate <provider>@<name>`).
    pub name: String,
    pub id: EndpointId,
    /// Last known addresses (relay URLs / IPs). Optional on the internet, where
    /// the id alone is resolved through address lookup.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub addrs: BTreeSet<TransportAddr>,
    /// Whether this peer may delegate LLM tasks to us (spending our accounts).
    #[serde(default)]
    pub can_delegate: bool,
    #[serde(default = "default_tasks_per_hour")]
    pub tasks_per_hour: u32,
}

impl TrustedPeer {
    pub fn addr(&self) -> EndpointAddr {
        EndpointAddr::from_parts(self.id, self.addrs.iter().cloned())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default)]
    pub peers: Vec<TrustedPeer>,
}

impl TrustStore {
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
        std::fs::write(path, serde_json::to_string_pretty(self)? + "\n")
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn get(&self, id: &EndpointId) -> Option<&TrustedPeer> {
        self.peers.iter().find(|p| &p.id == id)
    }

    /// Adds or updates a peer; an existing entry with the same id or name is replaced.
    pub fn allow(&mut self, peer: TrustedPeer) {
        self.peers
            .retain(|p| p.id != peer.id && p.name != peer.name);
        self.peers.push(peer);
        self.peers.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Removes the peer with this name or id; returns whether one was removed.
    pub fn remove(&mut self, name_or_id: &str) -> bool {
        let id = EndpointId::from_str(name_or_id).ok();
        let before = self.peers.len();
        self.peers
            .retain(|p| p.name != name_or_id && Some(p.id) != id);
        self.peers.len() != before
    }
}

const TICKET_PREFIX: &str = "p2pa1";

/// What one agent gives another so it can be allowlisted and dialed: its public
/// key, its addresses and its suggested name. Encoded as `p2pa1<base64url json>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub name: String,
    pub addr: EndpointAddr,
}

impl Ticket {
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("ticket serializes");
        format!(
            "{TICKET_PREFIX}{}",
            data_encoding::BASE64URL_NOPAD.encode(&json)
        )
    }

    /// Parses a ticket, or a bare hex/base32 endpoint id (no addresses, no name).
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if let Some(body) = s.strip_prefix(TICKET_PREFIX) {
            let json = data_encoding::BASE64URL_NOPAD
                .decode(body.as_bytes())
                .map_err(|e| anyhow!("invalid ticket: {e}"))?;
            return serde_json::from_slice(&json).context("invalid ticket");
        }
        match EndpointId::from_str(s) {
            Ok(id) => Ok(Self {
                name: String::new(),
                addr: EndpointAddr::new(id),
            }),
            Err(_) => bail!("not a p2pagents ticket or endpoint id: {s}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn peer(name: &str) -> TrustedPeer {
        TrustedPeer {
            name: name.into(),
            id: SecretKey::generate().public(),
            addrs: BTreeSet::new(),
            can_delegate: false,
            tasks_per_hour: DEFAULT_TASKS_PER_HOUR,
        }
    }

    #[test]
    fn ticket_round_trips_and_bare_ids_parse() {
        let id = SecretKey::generate().public();
        let ticket = Ticket {
            name: "bob".into(),
            addr: EndpointAddr::new(id).with_ip_addr("127.0.0.1:9".parse().unwrap()),
        };
        let encoded = ticket.encode();
        assert!(encoded.starts_with("p2pa1"));
        assert_eq!(Ticket::parse(&encoded).unwrap(), ticket);
        assert_eq!(
            Ticket::parse(&id.to_string()).unwrap().addr,
            EndpointAddr::new(id)
        );
        assert!(Ticket::parse("p2pa1!!").is_err());
        assert!(Ticket::parse("hello").is_err());
    }

    #[test]
    fn allow_replaces_same_id_or_name_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");
        let mut store = TrustStore::default();
        let bob = peer("bob");
        store.allow(bob.clone());
        store.allow(TrustedPeer {
            can_delegate: true,
            ..bob.clone()
        });
        store.allow(TrustedPeer {
            name: "bob".into(),
            ..peer("x")
        });
        assert_eq!(store.peers.len(), 1);
        assert_ne!(store.peers[0].id, bob.id);
        store.save(&path).unwrap();
        assert_eq!(TrustStore::load(&path).unwrap(), store);
        assert!(store.remove("bob"));
        assert!(!store.remove("bob"));
    }
}
