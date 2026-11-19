use p2paecn_board::{api, operator, rate, Shared, Source};
use std::sync::Arc;

/// P2PAECN_HOST: the host string signed into every request (wins over the bootstrap config's `host`).
/// P2PAECN_BIND: the public listener, default 127.0.0.1:8787.
/// P2PAECN_ADMIN_SOCKET: path of the operator channel's Unix socket; unset means no operator channel.
/// P2PAECN_CONFIG: optional bootstrap JSON (host, rooms, keys), read once at start; with a state file, only when
/// the state file does not exist yet.
/// P2PAECN_STATE: optional path of the state file. Unset: memory only, everything is lost on restart.
/// P2PAECN_WAIT_SECONDS: the longest `GET /v1/inbox/wait` holds a request, 1 to 28, default 25.
/// P2PAECN_LIMIT_KEY_REQUESTS, P2PAECN_LIMIT_KEY_WRITES, P2PAECN_LIMIT_ROOM_WRITES, P2PAECN_LIMIT_UNSIGNED,
/// P2PAECN_LIMIT_WINDOW_SECONDS, P2PAECN_SOURCE_HEADER: the rate limits (src/rate.rs).
#[tokio::main]
async fn main() {
    let start = api::now_unix();
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let host = env("P2PAECN_HOST");
    let bind = env("P2PAECN_BIND").unwrap_or_else(|| "127.0.0.1:8787".into());
    let state = env("P2PAECN_STATE").map(std::path::PathBuf::from);
    let (mut shared, source) =
        Shared::open(host, env("P2PAECN_CONFIG").map(Into::into), state, start).unwrap_or_else(|e| fail(&e));
    shared.limits = rate::Limiter::new(rate::Config::from_env(env).unwrap_or_else(|e| fail(&e)));
    if let Some(value) = env("P2PAECN_WAIT_SECONDS") {
        let range = 1..=p2paecn_board::WAIT_CAP_MAX;
        shared.wait_cap = value
            .parse()
            .ok()
            .filter(|n| range.contains(n))
            .unwrap_or_else(|| fail(&format!("P2PAECN_WAIT_SECONDS must be a whole number from 1 to {}", p2paecn_board::WAIT_CAP_MAX)));
    }
    let storage = match (&source, &shared.state) {
        (Source::Memory, _) | (_, None) => "in-memory store: every organisation, room, key and card is lost on restart".to_owned(),
        (source, Some(path)) => {
            let how = match source {
                Source::Loaded { config_ignored: true, chains_built: false } => "loaded from state file, P2PAECN_CONFIG ignored",
                Source::Loaded { config_ignored: false, chains_built: false } => "loaded from state file",
                Source::Loaded { config_ignored: true, chains_built: true } => {
                    "loaded from a version 1 state file, room records built from it and saved with the next change, P2PAECN_CONFIG ignored"
                }
                Source::Loaded { config_ignored: false, chains_built: true } => {
                    "loaded from a version 1 state file, room records built from it and saved with the next change"
                }
                Source::Created { from_config: true } => "new state file, from P2PAECN_CONFIG",
                _ => "new state file, empty",
            };
            format!("durable store: {} {how}; requests signed at or before {start} are refused", path.display())
        }
    };
    let shared = Arc::new(shared);
    let admin = env("P2PAECN_ADMIN_SOCKET");
    // The public port first: a second instance that cannot have it exits before touching the operator socket.
    let listener = tokio::net::TcpListener::bind(&bind).await.unwrap_or_else(|e| fail(&format!("cannot bind {bind}: {e}")));
    let bound = listener.local_addr().map_or(bind, |a| a.to_string());
    if let Some(path) = &admin {
        let socket = operator::bind(path.as_ref()).unwrap_or_else(|e| fail(&format!("cannot open operator socket {path}: {e}")));
        let router = operator::router(shared.clone());
        tokio::spawn(async move {
            if let Err(e) = axum::serve(socket, router).await {
                fail(&format!("operator channel stopped: {e}"));
            }
        });
    }
    {
        let store = shared.store.lock().unwrap();
        eprintln!(
            "p2paecn-board: host {}, {} orgs, {} rooms, {} keys, listening on {bound}, operator socket {} ({storage})",
            shared.host,
            store.orgs.len(),
            store.rooms.len(),
            store.keys.len(),
            admin.as_deref().unwrap_or("off"),
        );
        // A second line, so that the first keeps the shape it had.
        eprintln!("p2paecn-board: {}", shared.limits.config.describe());
    }
    if let Err(e) = api::serve(listener, api::router(shared), api::HEAD_TIMEOUT, api::REQUEST_TIMEOUT).await {
        fail(&format!("server stopped: {e}"));
    }
}

fn fail(message: &str) -> ! {
    eprintln!("p2paecn-board: {message}");
    std::process::exit(1)
}
