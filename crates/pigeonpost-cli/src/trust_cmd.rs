//! Client-side witnessed-registry trust import, status, and confirmed reset.

use std::io::Read;
use std::path::Path;

use pigeonpost_client::{
    Agent, RegistryTrustInput, RegistryTrustStatus, MAX_REGISTRY_TRUST_JSON_BYTES,
};
use pigeonpost_directory::private_store::read_trusted_file_bounded;
use zeroize::Zeroizing;

pub fn import(agent: &Agent, file: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let input = read_bundle(file)?;
    let status = agent.import_registry_trust(input)?;
    print_status(&status, json)?;
    Ok(())
}

pub fn status(agent: &Agent, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    match agent.registry_trust_status()? {
        Some(status) => print_status(&status, json)?,
        None if json => println!("{}", serde_json::json!({ "configured": false })),
        None => println!("no witnessed registry trust is configured"),
    }
    Ok(())
}

pub fn reset(
    agent: &Agent,
    confirmation: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    agent.reset_registry_trust(confirmation)?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "configured": false, "reset": true })
        );
    } else {
        println!("witnessed registry trust and derived state reset");
    }
    Ok(())
}

fn read_bundle(path: &Path) -> Result<RegistryTrustInput, Box<dyn std::error::Error>> {
    let bytes = if path == Path::new("-") {
        let mut bytes = Zeroizing::new(Vec::new());
        std::io::stdin()
            .lock()
            .take((MAX_REGISTRY_TRUST_JSON_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        bytes
    } else {
        read_trusted_file_bounded(path, MAX_REGISTRY_TRUST_JSON_BYTES as u64)?
    };
    Ok(RegistryTrustInput::from_json(&bytes)?)
}

fn print_status(
    status: &RegistryTrustStatus,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!(
            "{}",
            serde_json::json!({ "configured": true, "trust": status })
        );
        return Ok(());
    }

    let bundle = &status.bundle;
    println!("registry: {}", bundle.registry_url());
    println!("checkpoint origin: {}", bundle.origin());
    println!("checkpoint key: {}", bundle.checkpoint_key());
    println!(
        "witness threshold: {}/{}",
        bundle.witness_threshold(),
        bundle.witnesses().len()
    );
    for witness in bundle.witnesses() {
        println!("witness {} {}", witness.name, witness.public_key);
    }
    let minimum = bundle.minimum_checkpoint();
    println!("minimum checkpoint: {} {}", minimum.size, minimum.root);
    match &status.accepted_checkpoint {
        Some(checkpoint) => println!(
            "accepted checkpoint: {} {} (witnessed_at={}, fresh={})",
            checkpoint.size,
            checkpoint.root,
            status
                .witnessed_at
                .map_or_else(|| "none".into(), |value| value.to_string()),
            status.fresh
        ),
        None => println!("accepted checkpoint: none (fresh=false)"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use ed25519_dalek::SigningKey;
    use pigeonpost_client::REGISTRY_TRUST_BUNDLE_VERSION;
    use pigeonpost_directory::private_store::PrivateFile;
    use pigeonpost_registry::log::empty_root;

    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn trust_value() -> serde_json::Value {
        let checkpoint = SigningKey::from_bytes(&[1; 32]);
        let witness = SigningKey::from_bytes(&[2; 32]);
        serde_json::json!({
            "version": REGISTRY_TRUST_BUNDLE_VERSION,
            "registry_url": "https://registry.example",
            "origin": "registry.example/log",
            "checkpoint_key": hex(checkpoint.verifying_key().as_bytes()),
            "witnesses": [{
                "name": "independent.example/witness",
                "public_key": hex(witness.verifying_key().as_bytes()),
            }],
            "witness_threshold": 1,
            "minimum_checkpoint": { "size": 0, "root": hex(&empty_root()) },
            "max_cosignature_age_seconds": 600,
            "future_clock_skew_seconds": 30,
        })
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        let (private, _) = PrivateFile::open_or_create(path).unwrap();
        private.descriptor().set_len(0).unwrap();
        let mut descriptor = private.descriptor();
        descriptor.write_all(bytes).unwrap();
        descriptor.sync_all().unwrap();
    }

    #[test]
    fn cli_file_parser_uses_the_same_strict_bounded_bundle_contract() {
        let value = trust_value();
        let directory = crate::test_support::private_tempdir();
        let path = directory.path().join("private").join("trust.json");
        write_private(&path, &serde_json::to_vec(&value).unwrap());
        assert!(read_bundle(&path).is_ok());

        let mut unknown = value;
        unknown["unknown"] = serde_json::json!(true);
        write_private(&path, &serde_json::to_vec(&unknown).unwrap());
        assert!(read_bundle(&path).is_err());

        write_private(&path, &vec![b' '; MAX_REGISTRY_TRUST_JSON_BYTES + 1]);
        assert!(read_bundle(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn trust_file_rejects_links_special_files_and_mutable_ancestry() {
        use std::fs;
        use std::os::unix::fs::{symlink, PermissionsExt};

        let directory = crate::test_support::private_tempdir();
        let trusted = directory.path().join("trusted");
        fs::create_dir(&trusted).unwrap();
        fs::set_permissions(&trusted, fs::Permissions::from_mode(0o755)).unwrap();
        let source = trusted.join("bundle.json");
        let encoded = serde_json::to_vec(&trust_value()).unwrap();
        fs::write(&source, &encoded).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_bundle(&source).is_ok());

        let linked_file = trusted.join("linked.json");
        symlink(&source, &linked_file).unwrap();
        assert!(read_bundle(&linked_file).is_err());

        let alias = directory.path().join("alias");
        symlink(&trusted, &alias).unwrap();
        assert!(read_bundle(&alias.join("bundle.json")).is_err());

        let mutable = directory.path().join("mutable");
        fs::create_dir(&mutable).unwrap();
        fs::set_permissions(&mutable, fs::Permissions::from_mode(0o777)).unwrap();
        let mutable_bundle = mutable.join("bundle.json");
        fs::write(&mutable_bundle, &encoded).unwrap();
        assert!(read_bundle(&mutable_bundle).is_err());

        let fifo = trusted.join("bundle.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(read_bundle(&fifo).is_err());
    }
}
