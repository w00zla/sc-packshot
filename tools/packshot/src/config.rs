// Cluster configuration: <store>/packshot.json (in the bundle next to the exe). Without a token there is no network:
// packshot then works locally only. With a token every node listens on `listen`, announces itself by UDP broadcast on
// the same port (discovery) and connects to every node of the same token it hears; `peers` adds nodes outside the
// own network segment by address. Every node is known by its host name, which comes with the handshake.
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

pub const DEFAULT_PORT: u16 = 47950;

#[derive(Deserialize, Clone)]
pub struct NetConfig {
    /// this node's host name (not from the file)
    #[serde(skip)]
    pub name: String,
    /// shared secret of all machines; empty = no network
    #[serde(default)]
    pub token: String,
    /// what this node listens on, TCP for the connections and UDP for the discovery (default 0.0.0.0:47950)
    #[serde(default)]
    pub listen: String,
    /// nodes to connect to by address (host name or IP, port optional), for nodes the broadcast does not reach
    #[serde(default)]
    pub peers: Vec<String>,
    /// find the other nodes by UDP broadcast (default on)
    #[serde(default = "yes")]
    pub discovery: bool,
}

fn yes() -> bool {
    true
}

impl Default for NetConfig {
    fn default() -> Self {
        Self { name: String::new(), token: String::new(), listen: String::new(), peers: Vec::new(), discovery: true }
    }
}

impl NetConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let mut cfg: NetConfig = match fs::read(path) {
            Ok(b) => serde_json::from_slice(&b).with_context(|| path.display().to_string())?,
            Err(_) => NetConfig::default(),
        };
        cfg.name = sysinfo::System::host_name().ok().filter(|n| !n.is_empty()).unwrap_or_else(|| "packshot".into());
        if cfg.listen.trim().is_empty() {
            cfg.listen = format!("0.0.0.0:{DEFAULT_PORT}");
        }
        Ok(cfg)
    }

    pub fn enabled(&self) -> bool {
        !self.token.trim().is_empty()
    }

    /// The port of `listen` (TCP and the discovery's UDP)
    pub fn port(&self) -> u16 {
        self.listen.rsplit_once(':').and_then(|(_, p)| p.parse().ok()).unwrap_or(DEFAULT_PORT)
    }
}
