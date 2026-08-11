//! Race-safe, descriptor-verified persistence for service authentication keys.

use std::path::Path;

use pigeonpost_core::Identity;
use pigeonpost_directory::private_store::{load_or_create_secret32, load_secret32};
use zeroize::Zeroizing;

type LoadedSeed = (Zeroizing<[u8; 32]>, bool);

pub fn load_or_create(path: &Path) -> Result<(Identity, bool), Box<dyn std::error::Error>> {
    let (seed, created) = load_or_create_seed(path)?;
    Ok((Identity::from_seed(*seed), created))
}

/// Load an already provisioned secret without creating a replacement when it is absent.
pub fn load_existing_seed(path: &Path) -> Result<Zeroizing<[u8; 32]>, Box<dyn std::error::Error>> {
    Ok(load_secret32(path)?)
}

pub fn load_or_create_seed(path: &Path) -> Result<LoadedSeed, Box<dyn std::error::Error>> {
    let seed = Zeroizing::new(Identity::generate().to_seed());
    Ok(load_or_create_secret32(path, &seed)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_creators_converge_on_one_key() {
        let dir = crate::test_support::private_tempdir();
        let path = Arc::new(dir.path().join("private").join("loft.key"));
        let barrier = Arc::new(Barrier::new(8));
        let threads = (0..8)
            .map(|_| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    load_or_create(&path).unwrap().0.verifying_key().to_bytes()
                })
            })
            .collect::<Vec<_>>();
        let keys = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert!(keys.iter().all(|key| key == &keys[0]));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_keys_are_rejected() {
        use std::os::unix::fs::symlink;

        let dir = crate::test_support::private_tempdir();
        let target = dir.path().join("target");
        fs::write(&target, [0_u8; 32]).unwrap();
        let path = dir.path().join("loft.key");
        symlink(&target, &path).unwrap();
        assert!(load_or_create(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn existing_keys_with_unsafe_permissions_are_rejected_not_rewritten() {
        use std::os::unix::fs::PermissionsExt;

        let dir = crate::test_support::private_tempdir();
        let path = dir.path().join("loft.key");
        fs::write(&path, [7u8; 32]).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_or_create(&path).is_err());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn oversized_and_all_zero_keys_are_rejected() {
        let dir = crate::test_support::private_tempdir();
        let path = dir.path().join("private").join("loft.key");
        let (_, created) = load_or_create(&path).unwrap();
        assert!(created);

        fs::write(&path, [7u8; 33]).unwrap();
        let error = match load_or_create(&path) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("oversized loft key was accepted"),
        };
        assert!(error.contains("exactly 32 bytes"), "{error}");

        fs::write(&path, [0u8; 32]).unwrap();
        let error = match load_or_create(&path) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("all-zero loft key was accepted"),
        };
        assert!(error.contains("all-zero seed"), "{error}");
    }
}
