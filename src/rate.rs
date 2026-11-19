//! Rate limits, kept in memory. Each limit is a count per window (60 seconds unless configured): a fixed window
//! that starts with the first request counted in it. Nothing here is saved, so a restart forgets every count.
//!
//! - per key: all signed requests, and separately its writes;
//! - per room: the writes of all its members together;
//! - per source address: enroll attempts and requests that get the 401.
//!
//! A signed caller over a limit gets `429 rate_limited` with `Retry-After`. That answer is given only after the
//! signature has verified, and before the nonce is recorded, so the same request can be sent again. An unsigned
//! caller is never told anything: it keeps getting the identical 401 (see `api::unsigned_guard` for what changes
//! on the server side when a source address is over its limit).
//!
//! The source address is the address of the TCP peer. Behind a proxy that is the proxy, for every caller, unless
//! `P2PAECN_SOURCE_HEADER` names a header the proxy sets. No forwarding header is trusted by default.

use axum::http::{HeaderMap, HeaderName};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

/// More windows for source addresses than this are not kept. Past it, an address with no window of its own yet
/// shares one, so that a flood of made-up source addresses cannot grow the table without bound. Only this kind is
/// bounded this way, because only this kind is fed by callers nobody has checked: a key has a window only after
/// its signature verified, and a room only when a member wrote into it. A flood of addresses therefore never
/// pushes a key or a room into a shared window.
pub const MAX_WINDOWS: usize = 20_000;

/// Each limit is a count per window; 0 means no limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Signed requests of one key, reads and writes together.
    pub key_requests: u32,
    /// Writes of one key.
    pub key_writes: u32,
    /// Writes into one room, by all its members together.
    pub room_writes: u32,
    /// Enroll attempts and requests answered with the 401, from one source address.
    pub unsigned: u32,
    /// Seconds a window lasts.
    pub window: u64,
    /// A header whose last value is taken as the source address in place of the TCP peer's. `None`: no header is
    /// trusted.
    pub source_header: Option<HeaderName>,
}

impl Config {
    /// No limits: what a board built in code (`Shared::new`, `Shared::from_config_json`) starts with.
    pub const fn off() -> Self {
        Config { key_requests: 0, key_writes: 0, room_writes: 0, unsigned: 0, window: 60, source_header: None }
    }

    /// What the server binary uses when the environment sets nothing.
    pub const fn defaults() -> Self {
        Config { key_requests: 1200, key_writes: 60, room_writes: 240, unsigned: 120, window: 60, source_header: None }
    }

    /// `defaults()`, changed by `P2PAECN_LIMIT_KEY_REQUESTS`, `P2PAECN_LIMIT_KEY_WRITES`,
    /// `P2PAECN_LIMIT_ROOM_WRITES`, `P2PAECN_LIMIT_UNSIGNED` (whole numbers, 0 for no limit),
    /// `P2PAECN_LIMIT_WINDOW_SECONDS` (1 to 3600) and `P2PAECN_SOURCE_HEADER` (a header name). `get` reads one
    /// variable. A value that is not understood is an error: the board does not start on a limit it would ignore.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let mut config = Config::defaults();
        let number = |name: &str, max: u64, default: u64| -> Result<u64, String> {
            match get(name) {
                None => Ok(default),
                Some(v) => v.parse::<u64>().ok().filter(|n| *n <= max).ok_or(format!("{name} must be a whole number from 0 to {max}")),
            }
        };
        let max = u64::from(u32::MAX);
        config.key_requests = number("P2PAECN_LIMIT_KEY_REQUESTS", max, config.key_requests.into())? as u32;
        config.key_writes = number("P2PAECN_LIMIT_KEY_WRITES", max, config.key_writes.into())? as u32;
        config.room_writes = number("P2PAECN_LIMIT_ROOM_WRITES", max, config.room_writes.into())? as u32;
        config.unsigned = number("P2PAECN_LIMIT_UNSIGNED", max, config.unsigned.into())? as u32;
        config.window = number("P2PAECN_LIMIT_WINDOW_SECONDS", 3600, config.window)?;
        if config.window == 0 {
            return Err("P2PAECN_LIMIT_WINDOW_SECONDS must be from 1 to 3600".into());
        }
        if let Some(name) = get("P2PAECN_SOURCE_HEADER") {
            let header = HeaderName::from_bytes(name.to_lowercase().as_bytes()).map_err(|_| "P2PAECN_SOURCE_HEADER is not a header name".to_owned())?;
            config.source_header = Some(header);
        }
        Ok(config)
    }

    /// One line for the startup message.
    pub fn describe(&self) -> String {
        let n = |v: u32| if v == 0 { "none".to_owned() } else { v.to_string() };
        format!(
            "limits per {} s: {} requests and {} writes per key, {} writes per room, {} failed or enroll requests per source address ({})",
            self.window,
            n(self.key_requests),
            n(self.key_writes),
            n(self.room_writes),
            n(self.unsigned),
            match &self.source_header {
                Some(h) => format!("source address from the header {h}"),
                None => "source address is the TCP peer".to_owned(),
            }
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    KeyRequests,
    KeyWrites,
    RoomWrites,
    Unsigned,
}

struct Window {
    start: u64,
    count: u32,
}

#[derive(Default)]
struct Windows {
    map: HashMap<(Kind, String), Window>,
    /// How many of them are for source addresses.
    unsigned: usize,
    purged: u64,
}

pub struct Limiter {
    pub config: Config,
    windows: Mutex<Windows>,
}

impl Default for Limiter {
    fn default() -> Self {
        Limiter::new(Config::off())
    }
}

/// What a failed request did to its source address's count.
#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    Counted,
    /// This failure is the one that reached the limit: the moment to say so in the log, once.
    ReachedLimit,
    AlreadyOver,
}

impl Limiter {
    pub fn new(config: Config) -> Self {
        Limiter { config, windows: Mutex::new(Windows::default()) }
    }

    /// Seconds until the window that `w` is in ends; at least 1.
    fn retry_after(&self, w: &Window, now: u64) -> u64 {
        (w.start + self.config.window).saturating_sub(now).max(1)
    }

    /// The live window for a name, made if there is none. Windows that ended are dropped once per second.
    fn window<'a>(&self, windows: &'a mut Windows, kind: Kind, name: &str, now: u64) -> &'a mut Window {
        let length = self.config.window;
        if windows.purged != now {
            windows.map.retain(|_, w| w.start + length > now);
            windows.unsigned = windows.map.keys().filter(|(kind, _)| *kind == Kind::Unsigned).count();
            windows.purged = now;
        }
        // The shared window of the source addresses has the empty name, which no address has.
        let full = kind == Kind::Unsigned && windows.unsigned >= MAX_WINDOWS;
        let name = if full && !windows.map.contains_key(&(kind, name.to_owned())) { "" } else { name };
        let Windows { map, unsigned, .. } = windows;
        map.entry((kind, name.to_owned())).or_insert_with(|| {
            if kind == Kind::Unsigned {
                *unsigned += 1;
            }
            Window { start: now, count: 0 }
        })
    }

    /// Counts one unit in each of `buckets`, or in none: `Err` is the seconds to wait when one of them is full.
    fn take(&self, buckets: &[(Kind, &str, u32)], now: u64) -> Result<(), u64> {
        let mut windows = self.windows.lock().unwrap();
        let mut wait = 0;
        for (kind, name, limit) in buckets.iter().filter(|b| b.2 > 0) {
            let w = self.window(&mut windows, *kind, name, now);
            if w.count >= *limit {
                wait = wait.max(self.retry_after(w, now));
            }
        }
        if wait > 0 {
            return Err(wait);
        }
        for (kind, name, _) in buckets.iter().filter(|b| b.2 > 0) {
            self.window(&mut windows, *kind, name, now).count += 1;
        }
        Ok(())
    }

    /// One signed request of a key. `Err` is the seconds to wait.
    pub fn key_request(&self, key_id: &str, now: u64) -> Result<(), u64> {
        self.take(&[(Kind::KeyRequests, key_id, self.config.key_requests)], now)
    }

    /// One write of a key, into `room` when it is a write into a room. Counted for the key and for the room, or
    /// for neither. `Err` is the seconds to wait.
    pub fn write(&self, key_id: &str, room: Option<&str>, now: u64) -> Result<(), u64> {
        match room {
            Some(room) => self.take(&[(Kind::KeyWrites, key_id, self.config.key_writes), (Kind::RoomWrites, room, self.config.room_writes)], now),
            None => self.take(&[(Kind::KeyWrites, key_id, self.config.key_writes)], now),
        }
    }

    /// Whether a source address has used up its failures for this window.
    pub fn unsigned_over(&self, source: &str, now: u64) -> bool {
        let limit = self.config.unsigned;
        limit > 0 && self.window(&mut self.windows.lock().unwrap(), Kind::Unsigned, source, now).count >= limit
    }

    /// Counts one failed request (an answer of 401) for a source address.
    pub fn unsigned_failed(&self, source: &str, now: u64) -> Failed {
        let limit = self.config.unsigned;
        if limit == 0 {
            return Failed::Counted;
        }
        let mut windows = self.windows.lock().unwrap();
        let w = self.window(&mut windows, Kind::Unsigned, source, now);
        if w.count >= limit {
            return Failed::AlreadyOver;
        }
        w.count += 1;
        if w.count == limit {
            Failed::ReachedLimit
        } else {
            Failed::Counted
        }
    }

    /// The source address of a request: the last value of the configured header when it is an IP address, else
    /// the TCP peer, else `-` (a request that came through no listener, as in tests).
    pub fn source(&self, headers: &HeaderMap, peer: Option<IpAddr>) -> String {
        let from_header = self.config.source_header.as_ref().and_then(|name| {
            let last = headers.get_all(name).iter().next_back()?.to_str().ok()?;
            last.rsplit(',').next()?.trim().parse::<IpAddr>().ok()
        });
        from_header.or(peer).map_or_else(|| "-".to_owned(), |ip| ip.to_string())
    }

    /// How many windows are kept.
    pub fn window_count(&self) -> usize {
        self.windows.lock().unwrap().map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(config: Config) -> Limiter {
        Limiter::new(config)
    }

    #[test]
    fn a_window_fills_says_how_long_to_wait_and_starts_again() {
        let l = limiter(Config { key_requests: 3, window: 10, ..Config::off() });
        for _ in 0..3 {
            assert_eq!(l.key_request("reseller-x/agent", 100), Ok(()));
        }
        assert_eq!(l.key_request("reseller-x/agent", 100), Err(10));
        assert_eq!(l.key_request("reseller-x/agent", 104), Err(6));
        assert_eq!(l.key_request("reseller-x/agent", 109), Err(1));
        assert_eq!(l.key_request("supplier-a/agent", 109), Ok(()), "another key has its own window");
        assert_eq!(l.key_request("reseller-x/agent", 110), Ok(()), "the window ended");
        assert_eq!(l.key_request("reseller-x/agent", 111), Ok(()), "and the new one started at 110");
    }

    #[test]
    fn a_write_is_counted_for_key_and_room_or_for_neither() {
        let l = limiter(Config { key_writes: 2, room_writes: 3, ..Config::off() });
        assert_eq!(l.write("reseller-x/agent", Some("a-x"), 5), Ok(()));
        assert_eq!(l.write("reseller-x/agent", Some("a-x"), 5), Ok(()));
        // The key is full. The refusal does not use up the room.
        assert_eq!(l.write("reseller-x/agent", Some("a-x"), 5), Err(60));
        assert_eq!(l.write("supplier-a/agent", Some("a-x"), 5), Ok(()));
        // The room is full. The refusal does not use up the other key.
        assert_eq!(l.write("supplier-a/agent", Some("a-x"), 6), Err(59));
        assert_eq!(l.write("supplier-a/agent", Some("hub"), 6), Ok(()));
        assert_eq!(l.write("supplier-a/agent", None, 6), Err(59), "a write outside a room counts for the key");
        // Zero is no limit.
        let off = limiter(Config::off());
        for _ in 0..10_000 {
            assert_eq!(off.write("reseller-x/agent", Some("a-x"), 5), Ok(()));
        }
        assert_eq!(off.window_count(), 0);
    }

    #[test]
    fn failures_are_counted_per_source_and_the_limit_is_reached_once() {
        let l = limiter(Config { unsigned: 3, ..Config::off() });
        assert!(!l.unsigned_over("203.0.113.7", 1));
        assert_eq!(l.unsigned_failed("203.0.113.7", 1), Failed::Counted);
        assert_eq!(l.unsigned_failed("203.0.113.7", 1), Failed::Counted);
        assert_eq!(l.unsigned_failed("203.0.113.7", 1), Failed::ReachedLimit);
        assert_eq!(l.unsigned_failed("203.0.113.7", 2), Failed::AlreadyOver);
        assert!(l.unsigned_over("203.0.113.7", 2) && !l.unsigned_over("203.0.113.8", 2));
        assert!(!l.unsigned_over("203.0.113.7", 61));
    }

    #[test]
    fn the_table_is_bounded_and_expired_windows_are_dropped() {
        let l = limiter(Config { unsigned: 2, ..Config::off() });
        for n in 0..(MAX_WINDOWS + 500) {
            l.unsigned_failed(&format!("source-{n}"), 1);
        }
        assert_eq!(l.window_count(), MAX_WINDOWS + 1, "every source past the bound shares one window");
        assert!(l.unsigned_over("a-source-never-seen", 1), "which is full");
        assert!(!l.unsigned_over("source-1", 1), "a source with its own window keeps it");
        // The bound is on source addresses alone: with that table full, a key and a room still get windows of
        // their own, with their own limits.
        let l = limiter(Config { unsigned: 2, key_requests: 2, key_writes: 1, room_writes: 1, ..Config::off() });
        for n in 0..(MAX_WINDOWS + 10) {
            l.unsigned_failed(&format!("source-{n}"), 1);
        }
        for key in ["supplier-a/agent", "reseller-x/agent"] {
            assert_eq!((l.key_request(key, 1), l.key_request(key, 1), l.key_request(key, 1)), (Ok(()), Ok(()), Err(60)), "{key}");
        }
        assert_eq!((l.write("supplier-a/agent", Some("a-x"), 1), l.write("reseller-x/agent", Some("hub"), 1)), (Ok(()), Ok(())));
        assert_eq!(l.window_count(), MAX_WINDOWS + 1 + 6);
        let l = limiter(Config { unsigned: 2, ..Config::off() });
        for n in 0..(MAX_WINDOWS + 500) {
            l.unsigned_failed(&format!("source-{n}"), 1);
        }
        assert!(!l.unsigned_over("source-1", 61));
        assert_eq!(l.window_count(), 1, "the old windows are gone");
    }

    #[test]
    fn the_source_is_the_peer_unless_a_header_is_configured() {
        let peer: IpAddr = "198.51.100.4".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.append("x-forwarded-for", "203.0.113.7".parse().unwrap());
        headers.append("x-forwarded-for", "203.0.113.9, 203.0.113.8".parse().unwrap());
        let plain = limiter(Config::off());
        assert_eq!(plain.source(&headers, Some(peer)), "198.51.100.4", "no header is trusted by default");
        assert_eq!(plain.source(&headers, None), "-");
        let trusting = limiter(Config { source_header: Some(HeaderName::from_static("x-forwarded-for")), ..Config::off() });
        assert_eq!(trusting.source(&headers, Some(peer)), "203.0.113.8", "the last value: the one the nearest proxy wrote");
        assert_eq!(trusting.source(&HeaderMap::new(), Some(peer)), "198.51.100.4");
        let mut junk = HeaderMap::new();
        junk.append("x-forwarded-for", "not an address".parse().unwrap());
        assert_eq!(trusting.source(&junk, Some(peer)), "198.51.100.4");
    }

    #[test]
    fn the_environment_sets_the_limits_and_a_bad_value_is_an_error() {
        let env = |pairs: &'static [(&'static str, &'static str)]| move |name: &str| pairs.iter().find(|(n, _)| *n == name).map(|(_, v)| v.to_string());
        assert_eq!(Config::from_env(env(&[])), Ok(Config::defaults()));
        let set = Config::from_env(env(&[("P2PAECN_LIMIT_KEY_REQUESTS", "0"), ("P2PAECN_LIMIT_KEY_WRITES", "7"), ("P2PAECN_LIMIT_ROOM_WRITES", "8"),
            ("P2PAECN_LIMIT_UNSIGNED", "9"), ("P2PAECN_LIMIT_WINDOW_SECONDS", "2"), ("P2PAECN_SOURCE_HEADER", "X-Forwarded-For")]))
        .unwrap();
        assert_eq!(set, Config { key_requests: 0, key_writes: 7, room_writes: 8, unsigned: 9, window: 2, source_header: Some(HeaderName::from_static("x-forwarded-for")) });
        for bad in [("P2PAECN_LIMIT_KEY_WRITES", "many"), ("P2PAECN_LIMIT_UNSIGNED", "-1"), ("P2PAECN_LIMIT_WINDOW_SECONDS", "0"), ("P2PAECN_LIMIT_WINDOW_SECONDS", "3601"), ("P2PAECN_SOURCE_HEADER", "not a header")] {
            let pairs: &'static [(&'static str, &'static str)] = Box::leak(Box::new([bad]));
            assert!(Config::from_env(env(pairs)).is_err(), "{bad:?}");
        }
    }
}
