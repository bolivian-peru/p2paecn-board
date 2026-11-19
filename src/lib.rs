//! P2PAECN board server. See README.md for what exists and what does not.

pub mod api;
pub mod gate;
pub mod operator;
pub mod sig;
pub mod state;
pub mod store;

use serde::{Deserialize, Serialize};
use axum::http::StatusCode;
use std::path::PathBuf;
use std::sync::Mutex;
use store::{AgentKey, KeyStatus, Refusal, RoomKind, Store};

/// What an agent key may read from the other organisations in a room.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum KeyClass {
    /// Types, stages, ids and times only. For unattended loops and hosts with production access.
    Fields,
    /// Full text. For hosts the member declares free of production credentials.
    Text,
    /// A person's browser: reads what a `text` key reads, writes nothing. Every request that is not a `GET` is
    /// refused with `403 read_only_key` (see `api::authenticate`).
    Viewer,
}

impl KeyClass {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyClass::Fields => "fields",
            KeyClass::Text => "text",
            KeyClass::Viewer => "viewer",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fields" => Some(KeyClass::Fields),
            "text" => Some(KeyClass::Text),
            "viewer" => Some(KeyClass::Viewer),
            _ => None,
        }
    }
}

pub struct Shared {
    /// The board's own host name. Signatures are checked against this, never the Host header.
    pub host: String,
    /// Organisations, rooms, keys, codes, cards and messages. Everything that changes at run time.
    pub store: Mutex<Store>,
    /// `P2PAECN_STATE`: where every accepted change is saved before it is acknowledged. `None`: memory only.
    pub state: Option<PathBuf>,
    /// With a state file, the second this process started. Nonces are not saved, so a request accepted before a
    /// restart could otherwise be accepted again after it: every request whose `created` is at or before this
    /// second is refused as stale. `None` without a state file.
    pub not_before: Option<u64>,
}

/// A change that could not be saved: it is undone and not acknowledged.
pub const NOT_SAVED: Refusal = (StatusCode::INTERNAL_SERVER_ERROR, "state_not_saved");

/// Where the state of a starting board came from, for its startup line.
pub enum Source {
    /// No `P2PAECN_STATE`.
    Memory,
    /// The state file existed; `config_ignored` when `P2PAECN_CONFIG` was set too.
    Loaded { config_ignored: bool },
    /// The state file did not exist; it was written at start, from the config or empty.
    Created { from_config: bool },
}

#[derive(Deserialize)]
struct ConfigMember {
    org: String,
    handle: String,
}

/// `{id, orgs: [a, b]}` is a pair room; `{id, kind: "shared", members: [{org, handle}]}` a shared room.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigRoom {
    id: String,
    kind: Option<String>,
    orgs: Option<Vec<String>>,
    members: Option<Vec<ConfigMember>>,
}

#[derive(Deserialize)]
struct ConfigKey {
    id: String,
    org: String,
    class: KeyClass,
    public_key: String,
    expires: u64,
}

#[derive(Deserialize)]
struct Config {
    host: Option<String>,
    #[serde(default)]
    rooms: Vec<ConfigRoom>,
    #[serde(default)]
    keys: Vec<ConfigKey>,
}

impl Shared {
    /// An empty board: no organisations, rooms or keys until the operator adds them.
    pub fn new(host: String) -> Self {
        Shared { host, store: Mutex::new(Store::default()), state: None, not_before: None }
    }

    /// The board a process starts with. Without `state`: the bootstrap config, or an empty board, in memory only,
    /// as before. With `state`: the state file if it exists, and then the config is not read at all; otherwise the
    /// config (or an empty board), written to the state file at once, so that a path that cannot be written stops
    /// startup and the config is applied exactly once. `host` (`P2PAECN_HOST`) wins over the host saved in the
    /// state file, which wins over the config's. `start` is the second the process started.
    pub fn open(host: Option<String>, config: Option<PathBuf>, state: Option<PathBuf>, start: u64) -> Result<(Self, Source), String> {
        let read_config = |host: Option<String>| -> Result<Self, String> {
            match &config {
                Some(path) => {
                    let json = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                    Shared::from_config_json(&json, host)
                }
                None => Ok(Shared::new(host.ok_or("set P2PAECN_HOST (or give host in P2PAECN_CONFIG)")?)),
            }
        };
        let Some(path) = state else {
            return Ok((read_config(host)?, Source::Memory));
        };
        let (mut shared, source) = match state::load(&path).map_err(|e| format!("{e}; the board is not started and the file is left as it is"))? {
            Some((store, saved_host)) => {
                let host = host.or(Some(saved_host)).filter(|h| !h.is_empty()).ok_or("the state file holds no host: set P2PAECN_HOST")?;
                let shared = Shared { host, store: Mutex::new(store), state: None, not_before: None };
                (shared, Source::Loaded { config_ignored: config.is_some() })
            }
            None => {
                let shared = read_config(host)?;
                {
                    let store = shared.store.lock().unwrap();
                    state::save(&path, &store, &shared.host)
                        .map_err(|e| format!("cannot write state file {}: {e}; the board is not started", path.display()))?;
                }
                (shared, Source::Created { from_config: config.is_some() })
            }
        };
        shared.state = Some(path);
        shared.not_before = Some(start);
        Ok((shared, source))
    }

    /// A copy of the durable store to undo a change with, taken only when there is a file to save to.
    pub fn snapshot(&self, store: &Store) -> Option<Store> {
        self.state.as_ref().map(|_| store.durable_clone())
    }

    /// Saves the store after a change, before the change is acknowledged. When the save fails, the change is
    /// undone in memory, the reason logged, and `NOT_SAVED` returned for the caller to answer with. Called with the
    /// store's lock held from before the change to after the save, so saves happen in the order of the changes and
    /// nobody reads a change that is not yet saved.
    pub fn commit(&self, store: &mut Store, before: Option<Store>) -> Result<(), Refusal> {
        let (Some(path), Some(before)) = (&self.state, before) else {
            return Ok(());
        };
        match state::save(path, store, &self.host) {
            Ok(()) => Ok(()),
            Err(e) => {
                eprintln!("state not saved to {}: {e}; the change is undone and answered with 500 state_not_saved", path.display());
                store.restore(before);
                Err(NOT_SAVED)
            }
        }
    }

    /// Undoes whatever a refused change may have touched.
    pub fn undo(&self, store: &mut Store, before: Option<Store>) {
        if let Some(before) = before {
            store.restore(before);
        }
    }

    /// Builds the server state from a bootstrap JSON config, read once at start. `host` (from `P2PAECN_HOST`)
    /// wins over the config's own `host`; one of them is required. Keys listed here are active. Orgs are created
    /// from what rooms and keys mention. Refuses anything it cannot make sense of.
    pub fn from_config_json(json: &str, host: Option<String>) -> Result<Self, String> {
        let cfg: Config = serde_json::from_str(json).map_err(|e| format!("config: {e}"))?;
        let host = host.or(cfg.host).filter(|h| !h.is_empty()).ok_or("config: no host (set P2PAECN_HOST or give host)")?;
        let now = api::now_unix();
        let mut store = Store::default();

        let mut rooms = Vec::new();
        for r in cfg.rooms {
            let (kind, members): (RoomKind, Vec<(String, Option<String>)>) = match (r.kind.as_deref(), r.orgs, r.members) {
                (None | Some("pair"), Some(orgs), None) => (RoomKind::Pair, orgs.into_iter().map(|o| (o, None)).collect()),
                (Some("shared"), None, Some(members)) => {
                    (RoomKind::Shared, members.into_iter().map(|m| (m.org, Some(m.handle))).collect())
                }
                _ => return Err(format!("room {}: give orgs for a pair room, or kind \"shared\" and members", r.id)),
            };
            rooms.push((r.id, kind, members));
        }
        let mentioned = rooms.iter().flat_map(|(_, _, m)| m.iter().map(|(org, _)| org)).chain(cfg.keys.iter().map(|k| &k.org));
        for org in mentioned {
            if !store.orgs.contains(org) {
                store.add_org(org).map_err(|(_, code)| format!("org {org}: {code}"))?;
            }
        }
        for (id, kind, members) in rooms {
            store.add_room(&id, kind, &members).map_err(|(_, code)| format!("room {id}: {code}"))?;
        }
        for k in cfg.keys {
            let name_ok = k.id.strip_prefix(&format!("{}/", k.org)).is_some_and(store::is_key_name);
            if !name_ok {
                return Err(format!("key {}: the id is <org>/<name>, name of 2 to 32 of a-z, 0-9 and -", k.id));
            }
            let key = sig::public_key_from_b64url(&k.public_key)
                .ok_or_else(|| format!("key {}: public_key is not a base64url Ed25519 key", k.id))?;
            let agent = AgentKey { org: k.org, class: k.class, key, status: KeyStatus::Active, created: now, expires: k.expires, role: None, label: None };
            if store.keys.insert(k.id.clone(), agent).is_some() {
                return Err(format!("key {}: listed twice", k.id));
            }
        }
        Ok(Shared { host, store: Mutex::new(store), state: None, not_before: None })
    }
}
