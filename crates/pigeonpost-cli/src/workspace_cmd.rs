//! Workspace context — what a mailbox is *for*, encrypted so only its owner can read it.
//!
//! A mesh of twenty agents is only navigable if you can ask "which one works on that repo, and
//! where does it live?". That answer names a git repository, a machine and a filesystem path, so
//! it is exactly the sort of thing that should not sit in plaintext on somebody else's server: it
//! is a map of where your work is.
//!
//! So the encryption happens here, in the client. The postbox stores a nonce, a ciphertext and a
//! salt, and holds no key at any point — an operator reading the database learns that a mailbox
//! has context and roughly how big it is, and nothing else.
//!
//! The key comes from a passphrase through Argon2id rather than from device key material, because
//! the requirement is that *any* of the owner's machines can read it. A device key would mean the
//! machine that wrote the context is the only one that can read it, which defeats the purpose. The
//! cost is a passphrase the owner has to remember, and that cost is stated rather than hidden.

use std::path::Path;

use argon2::Argon2;
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    Key, XChaCha20Poly1305, XNonce,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

type Error = Box<dyn std::error::Error>;

/// What a mailbox is working on. Every field optional: a mailbox that only wants to record its job
/// title should not have to invent a repository.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
pub struct Workspace {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl Workspace {
    pub fn is_empty(&self) -> bool {
        *self == Workspace::default()
    }

    /// Merge `other` over `self`, keeping existing values where the update says nothing. This is
    /// what makes `workspace set --job-title X` an edit rather than a silent wipe of the rest.
    pub fn merged_with(mut self, other: Workspace) -> Self {
        let Workspace {
            git_repo,
            job_title,
            job_description,
            machine,
            local_path,
            notes,
        } = other;
        if git_repo.is_some() {
            self.git_repo = git_repo;
        }
        if job_title.is_some() {
            self.job_title = job_title;
        }
        if job_description.is_some() {
            self.job_description = job_description;
        }
        if machine.is_some() {
            self.machine = machine;
        }
        if local_path.is_some() {
            self.local_path = local_path;
        }
        if notes.is_some() {
            self.notes = notes;
        }
        self
    }
}

/// Argon2id parameters. The defaults from the `argon2` crate follow the RFC 9106 recommendation;
/// pinning them here means a future default change cannot silently strand everybody's existing
/// ciphertext behind a key that no longer derives.
fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; 32], Error> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| format!("could not derive a workspace key: {e}"))?;
    Ok(key)
}

/// Encrypt `workspace` for `address`.
///
/// The address is bound in as additional authenticated data, so a ciphertext copied from one
/// mailbox's row into another's fails to open rather than silently describing the wrong machine.
pub fn seal(
    workspace: &Workspace,
    address: &str,
    passphrase: &str,
    salt: &[u8],
    nonce: &[u8; 24],
) -> Result<Vec<u8>, Error> {
    let mut key = derive_key(passphrase, salt)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    let plaintext = serde_json::to_vec(workspace)?;
    let sealed = cipher
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: &plaintext,
                aad: address.as_bytes(),
            },
        )
        .map_err(|_| "could not encrypt the workspace context")?;
    key.zeroize();
    Ok(sealed)
}

/// Decrypt what [`seal`] produced. A wrong passphrase is indistinguishable from a tampered
/// ciphertext, which is the correct behaviour and also why the error says both.
pub fn open(
    ciphertext: &[u8],
    address: &str,
    passphrase: &str,
    salt: &[u8],
    nonce: &[u8],
) -> Result<Workspace, Error> {
    if nonce.len() != 24 {
        return Err("workspace context has a malformed nonce".into());
    }
    let mut key = derive_key(passphrase, salt)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: address.as_bytes(),
            },
        )
        .map_err(|_| "wrong passphrase, or the stored context has been tampered with");
    key.zeroize();
    Ok(serde_json::from_slice(&plaintext?)?)
}

/// Read a passphrase without echoing it, or take it from the environment for non-interactive use.
///
/// `PIGEONPOST_WORKSPACE_PASSPHRASE` exists so an agent can be given the key material deliberately
/// by whoever runs it. It is read from the environment rather than accepted as an argument,
/// because an argument would land in `ps` output and shell history.
pub fn passphrase(confirm: bool) -> Result<String, Error> {
    if let Ok(value) = std::env::var("PIGEONPOST_WORKSPACE_PASSPHRASE") {
        if !value.is_empty() {
            return Ok(value);
        }
    }
    let entered = rpassword::prompt_password("workspace passphrase: ")?;
    if entered.is_empty() {
        return Err("an empty passphrase would leave this readable by anyone".into());
    }
    if confirm {
        let again = rpassword::prompt_password("confirm passphrase: ")?;
        if again != entered {
            return Err("the two passphrases did not match".into());
        }
    }
    Ok(entered)
}

/// Where a machine remembers which salt a mailbox uses, so `show` need not ask twice.
pub fn describe(workspace: &Workspace) -> String {
    let mut lines = Vec::new();
    let mut row = |label: &str, value: &Option<String>| {
        if let Some(value) = value {
            lines.push(format!("  {label:<16}{value}"));
        }
    };
    row("job title", &workspace.job_title);
    row("description", &workspace.job_description);
    row("git repo", &workspace.git_repo);
    row("machine", &workspace.machine);
    row("local path", &workspace.local_path);
    row("notes", &workspace.notes);
    if lines.is_empty() {
        return "  (empty)".to_string();
    }
    lines.join("\n")
}

/// A per-mailbox salt, generated once and stored beside the ciphertext.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    use rand_core::RngCore;
    let mut out = [0u8; N];
    rand_core::OsRng.fill_bytes(&mut out);
    out
}

/// Best-effort machine name, so `--machine` need not be typed on every box.
pub fn this_machine() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
        })
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Best-effort git remote for a directory, so `--git-repo` need not be typed either.
pub fn git_remote(dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["-C", dir.to_str()?, "remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Workspace {
        Workspace {
            git_repo: Some("git@github.com:bekirdag/pigeonpost.git".into()),
            job_title: Some("main developer".into()),
            local_path: Some("/Users/bekir/Documents/apps/generic".into()),
            ..Default::default()
        }
    }

    #[test]
    fn a_sealed_workspace_opens_with_the_same_passphrase() {
        let salt = [7u8; 16];
        let nonce = [9u8; 24];
        let sealed = seal(&sample(), "/bekir/agent1", "correct horse", &salt, &nonce).unwrap();
        assert_ne!(sealed, serde_json::to_vec(&sample()).unwrap());
        let opened = open(&sealed, "/bekir/agent1", "correct horse", &salt, &nonce).unwrap();
        assert_eq!(opened, sample());
    }

    #[test]
    fn the_wrong_passphrase_does_not_open_it() {
        let salt = [7u8; 16];
        let nonce = [9u8; 24];
        let sealed = seal(&sample(), "/bekir/agent1", "correct horse", &salt, &nonce).unwrap();
        assert!(open(&sealed, "/bekir/agent1", "battery staple", &salt, &nonce).is_err());
        // A different salt is a different key even with the right passphrase.
        assert!(open(
            &sealed,
            "/bekir/agent1",
            "correct horse",
            &[8u8; 16],
            &nonce
        )
        .is_err());
    }

    #[test]
    fn context_cannot_be_moved_between_mailboxes() {
        // The address is authenticated, so a blob lifted from one row into another fails to open
        // instead of quietly telling you about somebody else's machine.
        let salt = [7u8; 16];
        let nonce = [9u8; 24];
        let sealed = seal(&sample(), "/bekir/agent1", "correct horse", &salt, &nonce).unwrap();
        assert!(open(&sealed, "/bekir/agent2", "correct horse", &salt, &nonce).is_err());
    }

    #[test]
    fn tampering_is_detected() {
        let salt = [7u8; 16];
        let nonce = [9u8; 24];
        let mut sealed = seal(&sample(), "/bekir/agent1", "correct horse", &salt, &nonce).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(open(&sealed, "/bekir/agent1", "correct horse", &salt, &nonce).is_err());
    }

    #[test]
    fn an_update_edits_rather_than_replaces() {
        let updated = sample().merged_with(Workspace {
            job_title: Some("bug fixer".into()),
            ..Default::default()
        });
        assert_eq!(updated.job_title.as_deref(), Some("bug fixer"));
        assert_eq!(
            updated.git_repo.as_deref(),
            Some("git@github.com:bekirdag/pigeonpost.git"),
            "setting one field must not wipe the others"
        );
    }

    #[test]
    fn a_malformed_nonce_is_refused_before_deriving_a_key() {
        assert!(open(b"x", "/bekir/agent1", "p", &[1u8; 16], &[0u8; 12]).is_err());
    }
}
