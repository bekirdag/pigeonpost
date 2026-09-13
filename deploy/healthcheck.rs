//! Loopback-only HTTP probe for the distroless production image.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

fn probe(port: u16, path: &str) -> io::Result<()> {
    let timeout = Duration::from_secs(2);
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )?;
    let mut status = [0; 13];
    stream.read_exact(&mut status)?;
    if status == *b"HTTP/1.1 200 " || status == *b"HTTP/1.0 200 " {
        Ok(())
    } else {
        Err(io::Error::other("service did not return HTTP 200"))
    }
}

fn main() -> ExitCode {
    let (port, path) = match std::env::args().nth(1).as_deref() {
        Some("registry") => (7718, "/health"),
        Some("directory") => (7719, "/ready"),
        Some("loft") => (7717, "/ready"),
        _ => return ExitCode::from(2),
    };
    match probe(port, path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("healthcheck: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn readiness_requires_a_complete_success_status() {
        for (response, healthy) in [
            ("HTTP/1.1 200 OK\r\n\r\nready", true),
            ("HTTP/1.0 200 OK\r\n\r\nready", true),
            ("HTTP/1.1 503 Service Unavailable\r\n\r\n", false),
            ("HTTP/1.1 2000 Invalid\r\n\r\n", false),
            ("HTTP/1.1 20", false),
        ] {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 256);
                }
                assert!(request.starts_with(b"GET /ready HTTP/1.1\r\n"));
                stream.write_all(response.as_bytes()).unwrap();
            });
            assert_eq!(probe(port, "/ready").is_ok(), healthy);
            server.join().unwrap();
        }
    }
}
