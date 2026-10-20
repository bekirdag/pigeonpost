//! Randomness for this crate's own keys and nonces, from the same CSPRNG OpenMLS uses.

use openmls_rust_crypto::RustCrypto;
use openmls_traits::random::OpenMlsRand;

use crate::error::{Error, Result};

pub(crate) fn random_array<const N: usize>() -> Result<[u8; N]> {
    thread_local! {
        static RNG: RustCrypto = RustCrypto::default();
    }
    RNG.with(|rng| rng.random_array::<N>())
        .map_err(|_| Error::invalid("system randomness unavailable"))
}
