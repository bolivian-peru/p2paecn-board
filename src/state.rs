//! Durable state in one file, `P2PAECN_STATE`. The whole durable store is written after every accepted change and
//! before the answer is sent: to a temporary file of mode 0600 in the same directory, synced, renamed over the
//! target, and the directory synced. A reader of the target therefore sees either the old file or the new one,
//! never half of one, which is also what makes a plain copy of it a safe backup.
//!
//! At start the file is loaded only if it is a regular file that neither group nor others can read or write, and
//! parses completely in the expected format and version. Anything else stops startup: the board never starts
//! empty over a damaged file, and never rewrites it.
//!
//! Not written: nonces. See `Shared::not_before` for how replay protection holds across a restart without them.

use crate::chain::{self, Entry};
use crate::store::{AgentKey, Card, EnrollCode, Message, Replay, Room, Store};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const FORMAT: &str = "p2paecn-board-state";
/// Raised whenever the meaning of a field changes. A file of a version this server does not know is refused, not
/// guessed at. Version 2 added `chains`, the hash-chained record of each room (`src/chain.rs`).
pub const VERSION: u32 = 2;
/// The version before chains. Such a file is still loaded: the chains are built from its cards and messages
/// (`chain::import`), in memory, and the file is written as version 2 with the next accepted change.
pub const VERSION_WITHOUT_CHAINS: u32 = 1;

#[derive(Serialize)]
struct ReplayOut<'a> {
    key_id: &'a str,
    idempotency_key: &'a str,
    fingerprint: &'a str,
    status: u16,
    body: &'a Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayIn {
    key_id: String,
    idempotency_key: String,
    fingerprint: String,
    status: u16,
    body: Value,
}

#[derive(Serialize)]
struct FileOut<'a> {
    format: &'static str,
    version: u32,
    host: &'a str,
    orgs: &'a BTreeSet<String>,
    rooms: Vec<&'a Room>,
    keys: &'a BTreeMap<String, AgentKey>,
    codes: &'a [EnrollCode],
    cards: &'a [Card],
    messages: &'a [Message],
    chains: &'a BTreeMap<String, Vec<Entry>>,
    replays: Vec<ReplayOut<'a>>,
}

/// Read first, on its own, so that a file of another format or version is named as such.
#[derive(Deserialize)]
struct Header {
    format: Option<String>,
    version: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIn {
    #[allow(dead_code)]
    format: String,
    version: u32,
    host: String,
    orgs: BTreeSet<String>,
    rooms: Vec<Room>,
    keys: BTreeMap<String, AgentKey>,
    codes: Vec<EnrollCode>,
    cards: Vec<Card>,
    messages: Vec<Message>,
    /// Present in version 2, absent in version 1.
    chains: Option<BTreeMap<String, Vec<Entry>>>,
    replays: Vec<ReplayIn>,
}

/// The file's bytes for `store`. Card and message order is kept as it is; replays are sorted so that the same
/// state always gives the same bytes.
pub fn encode(store: &Store, host: &str) -> Vec<u8> {
    let mut replays: Vec<ReplayOut> = store
        .replays
        .iter()
        .map(|((key_id, idem), r)| ReplayOut { key_id, idempotency_key: idem, fingerprint: &r.fingerprint, status: r.status.as_u16(), body: &r.body })
        .collect();
    replays.sort_by(|a, b| (a.key_id, a.idempotency_key).cmp(&(b.key_id, b.idempotency_key)));
    let file = FileOut {
        format: FORMAT,
        version: VERSION,
        host,
        orgs: &store.orgs,
        rooms: store.rooms.values().collect(),
        keys: &store.keys,
        codes: &store.codes,
        cards: &store.cards,
        messages: &store.messages,
        chains: &store.chains,
        replays,
    };
    let mut bytes = serde_json::to_vec(&file).expect("the store always serialises");
    bytes.push(b'\n');
    bytes
}

/// The store, the host string saved with it, and whether the chains were built here because the file is of the
/// version before them. A version 2 file must hold a chain for each of its rooms and for nothing else, and every
/// chain must link: each entry numbered in order, naming the hash of the one before, with its own hash right. That
/// catches a damaged file. It does not catch a deliberate change, which can recompute the hashes.
pub fn decode(bytes: &[u8]) -> Result<(Store, String, bool), String> {
    let header: Header = serde_json::from_slice(bytes).map_err(|e| format!("not a state file: {e}"))?;
    if header.format.as_deref() != Some(FORMAT) {
        return Err(format!("not a state file: format is not \"{FORMAT}\""));
    }
    if header.version != Some(VERSION) && header.version != Some(VERSION_WITHOUT_CHAINS) {
        return Err(format!("version {:?}, this server reads versions {VERSION_WITHOUT_CHAINS} and {VERSION} only", header.version));
    }
    let file: FileIn = serde_json::from_slice(bytes).map_err(|e| format!("damaged: {e}"))?;
    let mut store = Store::default();
    (store.orgs, store.keys, store.codes, store.cards, store.messages) = (file.orgs, file.keys, file.codes, file.cards, file.messages);
    for room in file.rooms {
        let id = room.id.clone();
        if store.rooms.insert(id.clone(), room).is_some() {
            return Err(format!("damaged: room {id} is listed twice"));
        }
    }
    let mut replays = HashMap::new();
    for r in file.replays {
        let status = StatusCode::from_u16(r.status).map_err(|_| format!("damaged: replay status {}", r.status))?;
        let slot = (r.key_id, r.idempotency_key);
        if replays.insert(slot.clone(), Replay { fingerprint: r.fingerprint, status, body: r.body }).is_some() {
            return Err(format!("damaged: replay {} {} is listed twice", slot.0, slot.1));
        }
    }
    store.replays = replays;
    let chains_built = match (file.version, file.chains) {
        (VERSION_WITHOUT_CHAINS, None) => {
            store.chains = chain::import(&store.rooms, &store.cards, &store.messages);
            true
        }
        (VERSION_WITHOUT_CHAINS, Some(_)) => return Err("damaged: a version 1 file with chains".into()),
        (_, None) => return Err("damaged: a version 2 file without chains".into()),
        (_, Some(chains)) => {
            if !chains.keys().eq(store.rooms.keys()) {
                return Err("damaged: the chains are not for exactly the rooms".into());
            }
            for (room, entries) in &chains {
                chain::check(room, entries).map_err(|seq| format!("damaged: the chain of room {room} does not link at entry {seq}"))?;
            }
            store.chains = chains;
            false
        }
    };
    Ok((store, file.host, chains_built))
}

/// The state file, or `None` when there is none yet. Every other outcome that is not a usable file is an error,
/// and the caller must not start.
pub fn load(path: &Path) -> Result<Option<(Store, String, bool)>, String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read state file {}: {e}", path.display())),
    };
    if !meta.file_type().is_file() {
        return Err(format!("state file {} is not a regular file", path.display()));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "state file {} has mode {mode:04o}: it must not be readable by group or others (nor writable). Run: chmod 600 {}",
            path.display(),
            path.display()
        ));
    }
    let bytes = fs::read(path).map_err(|e| format!("cannot read state file {}: {e}", path.display()))?;
    decode(&bytes).map(Some).map_err(|e| format!("cannot load state file {}: {e}", path.display()))
}

/// The directory a path is in; `.` for a bare file name.
fn directory(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// Writes `bytes` to `path` atomically: a temporary file of mode 0600 beside it, synced, renamed over it, and the
/// directory synced so the rename itself survives a power cut. On any error the target is left as it was, except
/// that a failure of the last step (the directory sync) leaves the new file in place, not yet known to be durable.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = directory(path);
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the state path has no file name"))?;
    let temp: PathBuf = dir.join(format!(".{}.tmp", name.to_string_lossy()));
    // A temporary file left by a crash is replaced; `mode` applies only to a file this call creates.
    match fs::remove_file(&temp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let written = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    File::open(dir)?.sync_all()
}

/// Saves the whole durable store.
pub fn save(path: &Path, store: &Store, host: &str) -> io::Result<()> {
    write_atomic(path, &encode(store, host))
}
