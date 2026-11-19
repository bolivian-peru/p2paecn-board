//! The real public listener: connections that stall are dropped, bytes that are not HTTP/1.1 are answered by the
//! HTTP library and not by the board, and every 401 leaves a reason in the log.
//! All three bind 127.0.0.1:0, a port the operating system picks.

mod common;

use common::*;
use p2paecn_board::api;
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Sends `bytes` and reads until the server closes the connection, for at most `limit`.
fn until_closed(addr: std::net::SocketAddr, bytes: &[u8], limit: Duration) -> (bool, Duration, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(limit)).unwrap();
    stream.write_all(bytes).unwrap();
    let started = Instant::now();
    let mut got = Vec::new();
    let closed = stream.read_to_end(&mut got).is_ok();
    (closed, started.elapsed(), String::from_utf8_lossy(&got).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_public_listener_drops_connections_that_stall() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let w = world(json!({ "host": HOST }));
    let server = tokio::spawn(api::serve(listener, w.app.clone(), Duration::from_secs(1), Duration::from_secs(2)));

    let stalls: [(&str, &[u8]); 5] = [
        ("silent", b""),
        ("half a request line", b"GET /v1/inb"),
        ("headers with no blank line", b"GET /v1/inbox HTTP/1.1\r\nHost: board.test\r\n"),
        ("a body shorter than its length", b"POST /v1/enroll HTTP/1.1\r\nHost: board.test\r\nContent-Length: 100\r\n\r\n{\"code\":"),
        ("idle after a whole request", b"GET / HTTP/1.1\r\nHost: board.test\r\n\r\n"),
    ];
    let results = tokio::task::spawn_blocking(move || {
        std::thread::scope(|s| {
            let running: Vec<_> = stalls.iter().map(|(what, bytes)| (*what, s.spawn(move || until_closed(addr, bytes, Duration::from_secs(8))))).collect();
            running.into_iter().map(|(what, t)| (what, t.join().unwrap())).collect::<Vec<_>>()
        })
    })
    .await
    .unwrap();
    server.abort();

    for (what, (closed, took, got)) in &results {
        assert!(*closed && *took < Duration::from_secs(6), "{what}: closed {closed} after {took:?}");
        match *what {
            "a body shorter than its length" => assert!(got.starts_with("HTTP/1.1 401") && got.ends_with("unauthorized\n"), "{what}: {got}"),
            "idle after a whole request" => assert!(got.starts_with("HTTP/1.1 200"), "{what}: {got}"),
            // Dropped without an answer: no status that the identical 401 does not already give.
            _ => assert!(got.is_empty(), "{what}: {got}"),
        }
    }
}

/// The identical 401 is the board's answer to a request it can read. Bytes that are not a readable HTTP/1.1
/// request never reach the router: the HTTP library answers them itself, with a bare 400, 414 or 431, or closes the
/// connection. This test pins that down, so that the documents can say exactly what a caller without a key sees. The
/// library's answer has no body and no header of the board, and it is the same for a route that exists and a path
/// that does not.
#[tokio::test(flavor = "multi_thread")]
async fn bytes_that_are_not_http_are_answered_by_the_library_and_say_nothing_about_the_board() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let w = world(json!({ "host": HOST }));
    let server = tokio::spawn(api::serve(listener, w.app.clone(), Duration::from_secs(1), Duration::from_secs(2)));

    let long_target = format!("GET /{} HTTP/1.1\r\nHost: board.test\r\nConnection: close\r\n\r\n", "a".repeat(70_000));
    let many_headers = |path: &str| {
        let extra: String = (0..150).map(|i| format!("X-{i}: a\r\n")).collect();
        format!("GET {path} HTTP/1.1\r\nHost: board.test\r\n{extra}Connection: close\r\n\r\n")
    };
    // (what, bytes, the status line the library sends, or "" when it sends nothing)
    let cases: Vec<(&str, String, &str)> = vec![
        ("a method that is not a token, on a route", "G@T /v1/inbox HTTP/1.1\r\nHost: board.test\r\n\r\n".into(), "HTTP/1.1 400 Bad Request"),
        ("a method that is not a token, on no route", "G@T /nothing-here HTTP/1.1\r\nHost: board.test\r\n\r\n".into(), "HTTP/1.1 400 Bad Request"),
        ("not a request line", "hello\r\n\r\n".into(), "HTTP/1.1 400 Bad Request"),
        ("an unknown HTTP version", "GET /v1/inbox HTTP/9.9\r\nHost: board.test\r\n\r\n".into(), "HTTP/1.1 400 Bad Request"),
        ("a header line without a colon", "GET /v1/inbox HTTP/1.1\r\nHost board.test\r\n\r\n".into(), "HTTP/1.1 400 Bad Request"),
        ("a content length that is not a number", "POST /v1/enroll HTTP/1.1\r\nHost: board.test\r\nContent-Length: abc\r\n\r\n".into(), "HTTP/1.1 400 Bad Request"),
        ("a request target of 70 000 bytes", long_target, "HTTP/1.1 414 URI Too Long"),
        ("150 header lines, on a route", many_headers("/v1/inbox"), "HTTP/1.1 431 Request Header Fields Too Large"),
        ("150 header lines, on no route", many_headers("/nothing-here"), "HTTP/1.1 431 Request Header Fields Too Large"),
        ("the HTTP/2 preface", "PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".into(), ""),
        // For comparison, requests the library can read: these reach the board, and get its 401.
        ("a readable request without a signature", "GET /v1/inbox HTTP/1.1\r\nHost: board.test\r\nConnection: close\r\n\r\n".into(), "HTTP/1.1 401 Unauthorized"),
        ("a method nobody defined", "FROB /v1/inbox HTTP/1.1\r\nHost: board.test\r\nConnection: close\r\n\r\n".into(), "HTTP/1.1 401 Unauthorized"),
    ];
    let results = tokio::task::spawn_blocking(move || {
        cases.into_iter().map(|(what, bytes, wanted)| (what, wanted, until_closed(addr, bytes.as_bytes(), Duration::from_secs(8)))).collect::<Vec<_>>()
    })
    .await
    .unwrap();
    server.abort();

    // Everything but the Date header, which changes with the clock.
    let without_date = |got: &str| got.lines().filter(|l| !l.to_lowercase().starts_with("date:")).collect::<Vec<_>>().join("\n");
    for (what, wanted, (closed, _, got)) in &results {
        assert!(*closed, "{what}: the connection stayed open");
        if wanted.is_empty() {
            assert!(got.is_empty(), "{what}: {got}");
        } else if wanted.contains("401") {
            assert!(got.starts_with(wanted) && got.ends_with("unauthorized\n"), "{what}: {got}");
        } else {
            // The library's own answer: a status line, `connection: close`, an empty body, a date. Nothing else.
            let (head, body) = got.split_once("\r\n\r\n").unwrap_or_else(|| panic!("{what}: {got}"));
            assert!(body.is_empty(), "{what}: a body: {got}");
            let mut names: Vec<String> = head.lines().skip(1).map(|l| l.split(':').next().unwrap().to_lowercase()).collect();
            names.sort();
            assert_eq!((head.lines().next().unwrap(), names), (*wanted, vec!["connection".to_owned(), "content-length".to_owned(), "date".to_owned()]), "{what}: {got}");
            assert!(head.to_lowercase().contains("content-length: 0"), "{what}: {got}");
        }
    }
    // The same malformed bytes get the same answer whether the path is a route or not.
    let answer = |what: &str| without_date(&results.iter().find(|(w, ..)| *w == what).unwrap().2 .2);
    assert_eq!(answer("a method that is not a token, on a route"), answer("a method that is not a token, on no route"));
    assert_eq!(answer("150 header lines, on a route"), answer("150 header lines, on no route"));
}

/// Kills the server when the test ends, also when it fails.
struct Server(std::process::Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Every 401 is the same on the wire, so the log is the only place an operator can see why.
#[test]
fn every_401_leaves_its_reason_in_the_log() {
    let child = Command::new(env!("CARGO_BIN_EXE_p2paecn-board"))
        .env("P2PAECN_HOST", HOST)
        .env("P2PAECN_BIND", "127.0.0.1:0")
        .env_remove("P2PAECN_ADMIN_SOCKET")
        .env_remove("P2PAECN_CONFIG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut server = Server(child);
    let stderr = server.0.stderr.take().unwrap();
    let (lines, log) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = lines.send(line);
        }
    });
    let started = log.recv_timeout(Duration::from_secs(20)).unwrap();
    let addr: std::net::SocketAddr = started.split("listening on ").nth(1).and_then(|r| r.split(',').next()).unwrap().parse().unwrap();

    let expected = [
        ("POST", "/", "no_route"), ("GET", "/v1/nothing-here", "no_route"), ("GET", "/v1/whoami/", "no_route"),
        ("DELETE", "/v1/whoami", "no_route"), ("PUT", "/v1/inbox", "no_route"), ("GET", "/v1/keys/revoke", "no_route"),
        ("GET", "/v1/enroll", "no_route"), ("GET", "/state", "no_route"), ("POST", "/orgs", "no_route"),
        ("POST", "/enroll-codes", "no_route"), ("POST", "/keys/activate", "no_route"), ("HEAD", "/v1/inbox", "no_route"),
        ("GET", "/v1/inbox", "missing_header"), ("POST", "/v1/enroll", "enroll_body_shape"),
    ];
    for (method, path, _) in expected {
        let request = format!("{method} {path} HTTP/1.1\r\nHost: board.test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let (closed, _, got) = until_closed(addr, request.as_bytes(), Duration::from_secs(10));
        assert!(closed && got.starts_with("HTTP/1.1 401"), "{method} {path}: {got}");
    }

    let mut refused = Vec::new();
    while refused.len() < expected.len() {
        match log.recv_timeout(Duration::from_secs(5)) {
            Ok(line) if line.starts_with("refused: ") => refused.push(line),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let wanted: Vec<String> = expected.iter().map(|(method, _, reason)| format!("refused: {reason} {method}")).collect();
    assert_eq!(refused, wanted);
}
