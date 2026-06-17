//! diyRAG launch gate — completion token **456468ann**.
//!
//! A local/LAN-only readiness aggregator (MASTER_BUILD_SPEC.md §16b, §1 #9).
//! It probes every diyRAG runtime item and serves:
//!   * `GET /456468ann` → 200 + the token **only when every item is ready**,
//!     otherwise 503 + the list of pending items.
//!   * `GET /healthz`    → 200 always (liveness).
//!   * `GET /readyz`     → 200 iff the gate is open, else 503.
//!
//! Security posture (LAN-only default): binds `127.0.0.1` by default and
//! REFUSES to bind a public address — only loopback or RFC1918 private ranges
//! are permitted (§12, and the user's local/LAN-only default). Zero external
//! crates: std only, so it builds and runs fully offline.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

/// The completion token. It is emitted/served ONLY when the gate is open
/// (every readiness item passed). Until then it never appears in any response.
const COMPLETION_TOKEN: &str = "456468ann";

const DEFAULT_BIND: &str = "127.0.0.1:8460";
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);
const MAX_REQUEST_BYTES: usize = 8 * 1024; // bound request reads (DoS guard)

// ---------------------------------------------------------------------------
// Readiness model
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Check {
    name: &'static str,
    kind: CheckKind,
}

#[derive(Clone)]
enum CheckKind {
    /// TCP connect probe against `host:port` (real runtime dependency).
    Tcp(String),
    /// Forced states (used by self-tests / demo and by "done" plan items).
    Ready,
    NotReady,
}

fn probe(c: &Check) -> bool {
    match &c.kind {
        CheckKind::Ready => true,
        CheckKind::NotReady => false,
        CheckKind::Tcp(addr) => tcp_ok(addr),
    }
}

fn tcp_ok(addr: &str) -> bool {
    match addr.to_socket_addrs() {
        Ok(mut it) => it.any(|sa| TcpStream::connect_timeout(&sa, PROBE_TIMEOUT).is_ok()),
        Err(_) => false,
    }
}

/// Evaluate every check, preserving order, into (name → ready).
fn evaluate(checks: &[Check]) -> BTreeMap<&'static str, bool> {
    checks.iter().map(|c| (c.name, probe(c))).collect()
}

#[derive(Debug, PartialEq, Eq)]
enum Gate {
    Open,
    Pending(Vec<&'static str>),
}

/// PURE: the gate opens iff *every* item is ready; otherwise it lists the
/// pending items. This is the single source of truth for "is 456468ann open?".
fn gate_from(results: &BTreeMap<&'static str, bool>) -> Gate {
    let pending: Vec<&'static str> = results
        .iter()
        .filter(|(_, ok)| !**ok)
        .map(|(name, _)| *name)
        .collect();
    if pending.is_empty() {
        Gate::Open
    } else {
        Gate::Pending(pending)
    }
}

// ---------------------------------------------------------------------------
// LAN-only bind policy
// ---------------------------------------------------------------------------

/// PURE: only loopback or RFC1918 private addresses may be bound. Public
/// addresses are rejected so the gate is never exposed to the internet by
/// default (the user's local/LAN-only default).
fn bind_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || is_rfc1918(v4),
        // Only loopback for IPv6; ULA handling is out of scope for v1.
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

fn is_rfc1918(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 10 || (o[0] == 172 && (16..=31).contains(&o[1])) || (o[0] == 192 && o[1] == 168)
}

// ---------------------------------------------------------------------------
// HTTP (hand-rolled, std-only)
// ---------------------------------------------------------------------------

fn http_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {len}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        status = status,
        len = body.len(),
        body = body,
    )
}

fn gate_body(gate: &Gate) -> (&'static str, String) {
    match gate {
        Gate::Open => (
            "200 OK",
            format!("{COMPLETION_TOKEN}\nstatus: OPEN — all plan items complete; diyRAG server is running.\n"),
        ),
        Gate::Pending(items) => (
            "503 Service Unavailable",
            format!(
                "status: PENDING — gate closed.\npending ({}):\n{}\n",
                items.len(),
                items.iter().map(|i| format!("  - {i}")).collect::<Vec<_>>().join("\n"),
            ),
        ),
    }
}

/// Parse the request target from the first line; serve the matching route.
fn respond(request_line: &str, checks: &[Check]) -> String {
    let target = request_line.split_whitespace().nth(1).unwrap_or("/");
    match target {
        "/healthz" => http_response("200 OK", "ok\n"),
        "/readyz" | "/456468ann" => {
            let gate = gate_from(&evaluate(checks));
            if target == "/readyz" {
                match gate {
                    Gate::Open => http_response("200 OK", "ready\n"),
                    Gate::Pending(_) => http_response("503 Service Unavailable", "not ready\n"),
                }
            } else {
                let (status, body) = gate_body(&gate);
                http_response(status, &body)
            }
        }
        _ => http_response("404 Not Found", "not found\n"),
    }
}

fn read_request_line(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = [0u8; MAX_REQUEST_BYTES];
    let n = stream.read(&mut buf)?;
    let text = String::from_utf8_lossy(&buf[..n]);
    Ok(text.lines().next().unwrap_or("").to_string())
}

fn handle(mut stream: TcpStream, checks: &[Check]) {
    let line = read_request_line(&mut stream).unwrap_or_default();
    let _ = stream.write_all(respond(&line, checks).as_bytes());
    let _ = stream.flush();
}

fn serve(bind: &str, checks: Vec<Check>) -> std::io::Result<()> {
    let sa: SocketAddr = bind
        .parse()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad bind address"))?;
    if !bind_allowed(sa.ip()) {
        eprintln!(
            "refusing to bind {sa}: local/LAN-only — use a loopback (127.0.0.1) or RFC1918 private address."
        );
        std::process::exit(2);
    }
    let listener = TcpListener::bind(sa)?;
    eprintln!("launch-gate listening on http://{sa}  (GET /{COMPLETION_TOKEN})");
    for conn in listener.incoming() {
        match conn {
            Ok(s) => handle(s, &checks),
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
    Ok(())
}

/// Default runtime checks: the diyRAG services the gate fronts. Hosts come from
/// the environment (compose service names), falling back to localhost ports.
fn default_checks() -> Vec<Check> {
    fn endpoint(env: &str, fallback: &str) -> String {
        std::env::var(env).unwrap_or_else(|_| fallback.to_string())
    }
    vec![
        Check {
            name: "postgres",
            kind: CheckKind::Tcp(endpoint("DIYRAG_PROBE_POSTGRES", "127.0.0.1:5432")),
        },
        Check {
            name: "qdrant",
            kind: CheckKind::Tcp(endpoint("DIYRAG_PROBE_QDRANT", "127.0.0.1:6333")),
        },
        Check {
            name: "nats",
            kind: CheckKind::Tcp(endpoint("DIYRAG_PROBE_NATS", "127.0.0.1:4222")),
        },
        Check {
            name: "minio",
            kind: CheckKind::Tcp(endpoint("DIYRAG_PROBE_MINIO", "127.0.0.1:9000")),
        },
        Check {
            name: "redis",
            kind: CheckKind::Tcp(endpoint("DIYRAG_PROBE_REDIS", "127.0.0.1:6379")),
        },
        Check {
            name: "api-gateway",
            kind: CheckKind::Tcp(endpoint("DIYRAG_PROBE_GATEWAY", "127.0.0.1:8443")),
        },
    ]
}

// ---------------------------------------------------------------------------
// In-process demo: prove the HTTP path end-to-end without external tools.
// ---------------------------------------------------------------------------

fn demo(all_ready: bool) -> std::io::Result<i32> {
    let checks = if all_ready {
        vec![
            Check {
                name: "plan:ingestion",
                kind: CheckKind::Ready,
            },
            Check {
                name: "plan:retrieval",
                kind: CheckKind::Ready,
            },
            Check {
                name: "plan:answer",
                kind: CheckKind::Ready,
            },
            Check {
                name: "runtime:server",
                kind: CheckKind::Ready,
            },
        ]
    } else {
        vec![
            Check {
                name: "plan:ingestion",
                kind: CheckKind::Ready,
            },
            Check {
                name: "plan:retrieval",
                kind: CheckKind::NotReady,
            },
            Check {
                name: "runtime:server",
                kind: CheckKind::NotReady,
            },
        ]
    };

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let server_checks = checks.clone();
    let handle = std::thread::spawn(move || {
        if let Ok((s, _)) = listener.accept() {
            super_handle(s, &server_checks);
        }
    });

    let mut client = TcpStream::connect(addr)?;
    client.write_all(
        format!("GET /{COMPLETION_TOKEN} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes(),
    )?;
    let mut resp = String::new();
    client.read_to_string(&mut resp)?;
    let _ = handle.join();

    let status_line = resp.lines().next().unwrap_or("");
    println!("--- request:  GET /{COMPLETION_TOKEN}  (all_ready={all_ready}) ---");
    println!("{resp}");
    let opened = status_line.contains("200") && resp.contains(COMPLETION_TOKEN);
    if all_ready {
        if opened {
            println!(">> GATE OPEN: {COMPLETION_TOKEN} served, server reachable.");
            Ok(0)
        } else {
            eprintln!(">> FAIL: expected open gate");
            Ok(1)
        }
    } else if status_line.contains("503") && !resp.contains(COMPLETION_TOKEN) {
        println!(">> GATE CLOSED as expected; token correctly withheld.");
        Ok(0)
    } else {
        eprintln!(">> FAIL: expected closed gate");
        Ok(1)
    }
}

// small shim so the demo thread can call `handle` (which consumes the stream)
fn super_handle(stream: TcpStream, checks: &[Check]) {
    handle(stream, checks);
}

fn main() {
    let arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "serve".to_string());
    let code = match arg.as_str() {
        "demo" => demo(true).unwrap_or(1),
        "demo-pending" => demo(false).unwrap_or(1),
        "serve" => {
            let bind = std::env::args()
                .skip_while(|a| a != "--bind")
                .nth(1)
                .unwrap_or_else(|| DEFAULT_BIND.to_string());
            match serve(&bind, default_checks()) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("serve error: {e}");
                    1
                }
            }
        }
        other => {
            eprintln!(
                "usage: launch-gate [serve [--bind ADDR] | demo | demo-pending]; got {other:?}"
            );
            2
        }
    };
    std::process::exit(code);
}

// ---------------------------------------------------------------------------
// Tests (strict: gate logic, token withholding, LAN-only bind policy)
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn results(pairs: &[(&'static str, bool)]) -> BTreeMap<&'static str, bool> {
        pairs.iter().cloned().collect()
    }

    #[test]
    fn gate_opens_only_when_all_ready() {
        let r = results(&[("a", true), ("b", true), ("c", true)]);
        assert_eq!(gate_from(&r), Gate::Open);
    }

    #[test]
    fn gate_pending_lists_every_failure_sorted() {
        let r = results(&[("a", true), ("c", false), ("b", false)]);
        match gate_from(&r) {
            Gate::Pending(p) => assert_eq!(p, vec!["b", "c"]), // BTreeMap → sorted
            Gate::Open => panic!("must not be open"),
        }
    }

    #[test]
    fn token_is_withheld_unless_open() {
        let closed = gate_body(&Gate::Pending(vec!["x"]));
        assert_eq!(closed.0, "503 Service Unavailable");
        assert!(
            !closed.1.contains(COMPLETION_TOKEN),
            "token must NOT leak while closed"
        );

        let open = gate_body(&Gate::Open);
        assert_eq!(open.0, "200 OK");
        assert!(
            open.1.contains(COMPLETION_TOKEN),
            "token must be served when open"
        );
    }

    #[test]
    fn route_456468ann_withholds_token_when_a_check_fails() {
        let checks = vec![
            Check {
                name: "ok",
                kind: CheckKind::Ready,
            },
            Check {
                name: "down",
                kind: CheckKind::NotReady,
            },
        ];
        let resp = respond("GET /456468ann HTTP/1.1", &checks);
        assert!(resp.contains("503"));
        assert!(!resp.contains(COMPLETION_TOKEN));
        assert!(resp.contains("down"));
    }

    #[test]
    fn route_456468ann_emits_token_when_all_ready() {
        let checks = vec![Check {
            name: "ok",
            kind: CheckKind::Ready,
        }];
        let resp = respond("GET /456468ann HTTP/1.1", &checks);
        assert!(resp.contains("200 OK"));
        assert!(resp.contains(COMPLETION_TOKEN));
    }

    #[test]
    fn healthz_is_always_ok() {
        assert!(respond("GET /healthz HTTP/1.1", &[]).contains("200 OK"));
    }

    #[test]
    fn lan_only_bind_policy() {
        use std::net::Ipv6Addr;
        // allowed: loopback + RFC1918
        assert!(bind_allowed(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(bind_allowed(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))));
        assert!(bind_allowed(IpAddr::V4(Ipv4Addr::new(172, 16, 3, 9))));
        assert!(bind_allowed(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50))));
        assert!(bind_allowed(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        // rejected: public
        assert!(!bind_allowed(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!bind_allowed(IpAddr::V4(Ipv4Addr::new(172, 32, 0, 1)))); // outside 16..=31
        assert!(!bind_allowed(IpAddr::V6(
            "2606:4700:4700::1111".parse().unwrap()
        )));
    }
}
