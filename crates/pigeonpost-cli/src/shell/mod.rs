//! Agent SSH (SDS §11, `docs/agent-ssh.md`): the pieces both ends share.
//!
//! * [`client`] is `pigeonpost ssh <agent>` — a person's terminal on an agent's workspace;
//! * [`agent`] is agentd's embedded SSH server, which listens on **no port**: it reaches the tunnel
//!   broker outbound, and serves exactly one session per broker connection;
//! * [`config`] is the machine owner's local opt-in (`[mailbox.shell]` in `agentd.toml`) and
//!   [`confine`] the boundary each session's process runs inside.
//!
//! Between the two ends the protocol is SSH, end to end. The broker splices WebSocket binary
//! messages and sees ciphertext only; the client pins the agent's host key; the agent accepts only
//! a short-lived certificate from the Pigeonpost SSH CA that names this exact session.

#[cfg(unix)]
pub mod agent;
pub mod client;
pub mod config;
#[cfg(unix)]
pub mod confine;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

/// The SSH user name every session uses (who connects is in the certificate).
pub const SSH_USER: &str = "pigeonpost";
/// The production broker. `PIGEONPOST_TUNNEL_URL` overrides it (loopback tests, self-hosting).
pub const DEFAULT_TUNNEL: &str = "wss://tunnel.pigeonpost.dev";
/// Bytes per WebSocket message, below the broker's 256 KiB limit.
const CHUNK: usize = 32 * 1024;

type Error = Box<dyn std::error::Error + Send + Sync>;

/// The broker origin this machine will connect to.
pub fn tunnel_origin() -> String {
    std::env::var("PIGEONPOST_TUNNEL_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_TUNNEL.to_string())
}

/// Whether `url` is exactly `<origin>/t/<session>`, on `wss` (or `ws` to loopback). The postbox
/// names the broker URL per session; it must not be able to send either end anywhere else.
pub fn broker_url_ok(url: &str, origin: &str, session_id: &str) -> bool {
    let secure = origin.starts_with("wss://")
        || origin.starts_with("ws://127.0.0.1:")
        || origin.starts_with("ws://localhost:")
        || origin.starts_with("ws://[::1]:");
    secure && valid_session_id(session_id) && url == format!("{origin}/t/{session_id}")
}

pub fn valid_session_id(id: &str) -> bool {
    id.len() == 30
        && id.starts_with("ssh_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

/// The way a tunnel ended, as the broker said in its close frame.
pub type CloseReason = tokio::sync::watch::Receiver<Option<String>>;

/// Connect one leg to the broker and present its one-time token. Returns a byte stream that is
/// the SSH transport, and the broker's close reason when it ends.
pub async fn dial(
    url: &str,
    leg_token: &str,
) -> Result<(tokio::io::DuplexStream, CloseReason), Error> {
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| format!("could not reach the tunnel broker: {e}"))?;
    ws.send(Message::Text(
        serde_json::json!({ "leg": leg_token }).to_string().into(),
    ))
    .await?;
    let (ours, theirs) = tokio::io::duplex(256 * 1024);
    let (reason_tx, reason_rx) = tokio::sync::watch::channel(None);
    tokio::spawn(async move {
        let (mut sink, mut stream) = ws.split();
        let (mut rd, mut wr) = tokio::io::split(theirs);
        let up = async {
            let mut buf = vec![0u8; CHUNK];
            loop {
                match rd.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sink
                            .send(Message::Binary(buf[..n].to_vec().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            let _ = sink.send(Message::Close(None)).await;
        };
        let down = async {
            while let Some(m) = stream.next().await {
                match m {
                    Ok(Message::Binary(b)) => {
                        if wr.write_all(&b).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        let _ = reason_tx.send(Some(
                            frame
                                .map(|f| f.reason.to_string())
                                .unwrap_or_else(|| "closed".into()),
                        ));
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            let _ = wr.shutdown().await;
        };
        tokio::select! { _ = up => {}, _ = down => {} }
    });
    Ok((ours, reason_rx))
}

/// SHA-256 fingerprint in OpenSSH's `SHA256:…` form.
pub fn fingerprint(key: &russh::keys::PublicKey) -> String {
    key.fingerprint(russh::keys::HashAlg::Sha256).to_string()
}

/// An Ed25519 private key from 32 random bytes.
pub fn new_ed25519() -> russh::keys::PrivateKey {
    use rand_core::RngCore;
    let mut seed = zeroize::Zeroizing::new([0u8; 32]);
    rand_core::OsRng.fill_bytes(&mut *seed);
    russh::keys::PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(
        &seed,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_urls_are_pinned_to_the_origin_and_the_session() {
        let sid = "ssh_abcdefghijklmnopqrstuvwxyz";
        let o = "wss://tunnel.pigeonpost.dev";
        assert!(broker_url_ok(&format!("{o}/t/{sid}"), o, sid));
        for bad in [
            format!("wss://evil.example/t/{sid}"),
            format!("{o}/t/ssh_bbbbbbbbbbbbbbbbbbbbbbbbbb"),
            format!("{o}/t/{sid}?x"),
            format!("{o}/x/{sid}"),
            format!("{o}/t/{sid}/"),
        ] {
            assert!(!broker_url_ok(&bad, o, sid), "{bad}");
        }
        // Plain ws only to loopback.
        assert!(!broker_url_ok(
            &format!("ws://tunnel.example/t/{sid}"),
            "ws://tunnel.example",
            sid
        ));
        assert!(broker_url_ok(
            &format!("ws://127.0.0.1:9/t/{sid}"),
            "ws://127.0.0.1:9",
            sid
        ));
        assert!(!broker_url_ok("x", o, "ssh_../../x"));
    }
}
