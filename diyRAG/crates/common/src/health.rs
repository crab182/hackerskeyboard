//! Process-level healthcheck for the `<binary> healthcheck` CLI form invoked by
//! container `HEALTHCHECK` directives (MASTER_BUILD_SPEC.md §16b, §12.8).
//!
//! Every diyRAG service binary recognises a first argument of `healthcheck` and,
//! instead of booting, runs a cheap probe and exits `0` (healthy) or `1`. This
//! lets one image serve both roles: `ENTRYPOINT` boots the service, and the
//! container `HEALTHCHECK` re-invokes the same binary with `healthcheck`.
//!
//! The probe is **std-only** (no async runtime, no HTTP client, no TLS): a plain
//! HTTP/1.0 `GET /healthz` over a loopback `TcpStream`. That keeps it usable from
//! a distroless image and avoids pulling the probe into the service's async stack.
//! Services that do not (yet) serve HTTP use [`liveness_ok`] instead — "the
//! process started and parsed its args" is the only signal available for a pure
//! worker until it grows a health port.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Exit code for a healthy probe (matches `EXIT_SUCCESS`).
pub const HEALTHY: i32 = 0;
/// Exit code for an unhealthy probe.
pub const UNHEALTHY: i32 = 1;

/// How long the loopback probe waits to connect / read before giving up.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The CLI subcommand a container `HEALTHCHECK` passes as the first argument.
pub const HEALTHCHECK_ARG: &str = "healthcheck";

/// True when this process was invoked as `<binary> healthcheck` (the container
/// `HEALTHCHECK` form). Services check this at the very top of `main` and run a
/// probe instead of booting the server.
#[must_use]
pub fn is_healthcheck_invocation() -> bool {
    std::env::args().nth(1).as_deref() == Some(HEALTHCHECK_ARG)
}

/// Liveness exit code for a service that does not serve HTTP yet: reaching this
/// point means the binary launched and parsed its arguments. Returns [`HEALTHY`].
#[must_use]
pub fn liveness_ok() -> i32 {
    HEALTHY
}

/// Rewrite a bind address (`0.0.0.0:8443`, `[::]:8443`, `host:8443`) to a
/// loopback target the in-container probe can reach. `None` when no valid port
/// can be parsed.
#[must_use]
pub fn loopback_target(bind_addr: &str) -> Option<String> {
    let port: u16 = bind_addr.rsplit(':').next()?.parse().ok()?;
    Some(format!("127.0.0.1:{port}"))
}

/// PURE: does an HTTP status line (`HTTP/1.1 200 OK`) indicate success (2xx)?
#[must_use]
pub fn status_line_ok(first_line: &str) -> bool {
    let mut parts = first_line.split_whitespace();
    let _version = parts.next();
    matches!(parts.next(), Some(code) if code.starts_with('2'))
}

/// Probe `GET /healthz` on the loopback port parsed from `bind_addr`, returning a
/// process exit code: [`HEALTHY`] iff a 2xx response arrives within the timeout,
/// else [`UNHEALTHY`]. Diagnostics go to stderr so they appear in `docker inspect`
/// health logs without polluting any stdout protocol stream.
#[must_use]
pub fn http_healthcheck(bind_addr: &str) -> i32 {
    let Some(target) = loopback_target(bind_addr) else {
        eprintln!("healthcheck: cannot derive a port from bind_addr `{bind_addr}`");
        return UNHEALTHY;
    };
    match probe_healthz(&target) {
        Ok(true) => HEALTHY,
        Ok(false) => {
            eprintln!("healthcheck: {target} /healthz did not return 2xx");
            UNHEALTHY
        }
        Err(e) => {
            eprintln!("healthcheck: {target}: {e}");
            UNHEALTHY
        }
    }
}

/// Open a short-lived connection to `target` and read the `/healthz` status line.
fn probe_healthz(target: &str) -> std::io::Result<bool> {
    let addr = target.to_socket_addrs()?.next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no socket address")
    })?;
    let mut stream = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT)?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT))?;
    // HTTP/1.0 + Connection: close so the server closes the socket and our
    // read_to_string terminates without needing to parse Content-Length.
    stream.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf)?;
    Ok(buf.lines().next().map(status_line_ok).unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn loopback_target_extracts_port_or_none() {
        assert_eq!(
            loopback_target("0.0.0.0:8443").as_deref(),
            Some("127.0.0.1:8443")
        );
        assert_eq!(
            loopback_target("[::]:9000").as_deref(),
            Some("127.0.0.1:9000")
        );
        assert_eq!(
            loopback_target("core-api:8081").as_deref(),
            Some("127.0.0.1:8081")
        );
        assert_eq!(loopback_target("no-port"), None);
        assert_eq!(loopback_target("host:notaport"), None);
    }

    #[test]
    fn status_line_ok_recognizes_2xx_only() {
        assert!(status_line_ok("HTTP/1.1 200 OK"));
        assert!(status_line_ok("HTTP/1.0 204 No Content"));
        assert!(!status_line_ok("HTTP/1.1 503 Service Unavailable"));
        assert!(!status_line_ok("HTTP/1.1 404 Not Found"));
        assert!(!status_line_ok(""));
    }

    #[test]
    fn http_healthcheck_is_healthy_against_a_200_server() {
        // Stand up a one-shot loopback server that answers /healthz with 200,
        // then probe it via the public entry point.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 256];
                let _ = s.read(&mut buf);
                let _ = s.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nok\n",
                );
            }
        });
        // Use a 0.0.0.0 bind form to exercise the loopback rewrite.
        assert_eq!(http_healthcheck(&format!("0.0.0.0:{port}")), HEALTHY);
        let _ = handle.join();
    }

    #[test]
    fn http_healthcheck_is_unhealthy_when_nothing_listens() {
        // Reserve a port, then drop the listener so the connect refuses.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert_eq!(http_healthcheck(&format!("127.0.0.1:{port}")), UNHEALTHY);
    }

    #[test]
    fn unparseable_bind_addr_is_unhealthy() {
        assert_eq!(http_healthcheck("no-port-here"), UNHEALTHY);
    }
}
