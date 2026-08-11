//! Loft-side attribution admission.
//!
//! The loft verifies only public facts. It never decrypts an attribution claim and never receives
//! a compliance private key. Key resolution is intentionally injected so registry caching and
//! freshness policy can evolve without coupling storage to a network client.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use pigeonpost_compliance_format::{
    attribution_epoch_contains, validate_attribution_epoch, ComplianceKeyId,
};
use pigeonpost_core::{envelope::Wrap, AttributionRequirement};
use pigeonpost_registry::ComplianceKeyStatus;
use sha2::{Digest, Sha256};

use crate::error::{LoftError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedAttributionKey {
    pub public_key: [u8; 32],
    pub not_before_ms: u64,
    /// Exclusive upper bound.
    pub not_after_ms: u64,
    pub status: ComplianceKeyStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum AttributionResolutionError {
    #[error("key cache unavailable")]
    Unavailable,
}

/// Cache-only lookup used on the ingest path. Implementations must not perform unbounded network
/// I/O here; refresh belongs in a separately supervised task.
pub trait AttributionKeyResolver: Send + Sync {
    /// Whether this node has a configured registry-backed key source.
    fn configured(&self) -> bool {
        true
    }

    fn resolve(
        &self,
        key_id: &ComplianceKeyId,
    ) -> std::result::Result<Option<ResolvedAttributionKey>, AttributionResolutionError>;

    /// Verify that the cache is initialized and fresh enough for fail-closed admission.
    fn readiness(&self, _now_ms: u64) -> std::result::Result<(), AttributionResolutionError> {
        Ok(())
    }

    /// Supervised refresh cadence. `None` denotes a manually maintained or fixed resolver.
    fn refresh_interval_ms(&self) -> Option<u64> {
        None
    }

    /// Refresh trusted state outside the publish request path.
    fn refresh(
        &self,
    ) -> Pin<
        Box<dyn Future<Output = std::result::Result<(), AttributionResolutionError>> + Send + '_>,
    > {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Default)]
pub struct UnconfiguredAttributionResolver;

impl AttributionKeyResolver for UnconfiguredAttributionResolver {
    fn configured(&self) -> bool {
        false
    }

    fn resolve(
        &self,
        _key_id: &ComplianceKeyId,
    ) -> std::result::Result<Option<ResolvedAttributionKey>, AttributionResolutionError> {
        Ok(None)
    }
}

pub(crate) fn validate(
    wrap: &Wrap,
    requirement: Option<&AttributionRequirement>,
    resolver: &Arc<dyn AttributionKeyResolver>,
    now_ms: u64,
) -> Result<()> {
    let Some(block) = wrap.attribution.as_ref() else {
        return if requirement.is_some() {
            Err(LoftError::AttributionRejected)
        } else {
            Ok(())
        };
    };
    let Some(block) = block.as_v3() else {
        return Err(LoftError::AttributionRejected);
    };
    if attribution_epoch_contains(&block.key_id, now_ms).is_err() {
        return Err(LoftError::AttributionRejected);
    }
    if requirement.is_some_and(|required| !required.matches_key_id(&block.key_id)) {
        return Err(LoftError::AttributionRejected);
    }

    // A new attribution escrow is valid only against fresh witnessed state. Nodes without that
    // state may still admit unattributed wraps for optional policies, but never an unchecked block.
    if !resolver.configured() {
        return Err(LoftError::AttributionUnavailable);
    }
    resolver
        .readiness(now_ms)
        .map_err(|_| LoftError::AttributionUnavailable)?;

    let resolved = resolver
        .resolve(&block.key_id)
        .map_err(|_| LoftError::AttributionUnavailable)?
        // A cache is a witnessed prefix, not proof that the Registry will never append this key.
        // Preserve the immutable sender wrap so a supervised refresh can decide it later.
        .ok_or(LoftError::AttributionUnavailable)?;
    if resolved.status != ComplianceKeyStatus::Active
        || validate_attribution_epoch(&block.key_id, resolved.not_before_ms, resolved.not_after_ms)
            .is_err()
        || attribution_epoch_contains(&block.key_id, now_ms) != Ok(true)
    {
        return Err(LoftError::AttributionRejected);
    }
    let digest: [u8; 32] = Sha256::digest(resolved.public_key).into();
    if digest != block.compliance_key_digest {
        return Err(LoftError::AttributionRejected);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigeonpost_compliance_format::{ComplianceKeyId, CompliancePurpose, Jurisdiction};
    use pigeonpost_core::{envelope, Identity};

    const AUGUST_2026: u64 = 1_785_542_400_000;
    const SEPTEMBER_2026: u64 = 1_788_220_800_000;
    const NOW_MS: u64 = 1_786_105_721_000;
    const NOW_SECS: u64 = NOW_MS / 1_000;

    struct FixedResolver {
        key: Option<ResolvedAttributionKey>,
    }

    impl AttributionKeyResolver for FixedResolver {
        fn resolve(
            &self,
            _key_id: &ComplianceKeyId,
        ) -> std::result::Result<Option<ResolvedAttributionKey>, AttributionResolutionError>
        {
            Ok(self.key)
        }
    }

    struct UnavailableResolver;

    impl AttributionKeyResolver for UnavailableResolver {
        fn readiness(&self, _now_ms: u64) -> std::result::Result<(), AttributionResolutionError> {
            Err(AttributionResolutionError::Unavailable)
        }

        fn resolve(
            &self,
            _key_id: &ComplianceKeyId,
        ) -> std::result::Result<Option<ResolvedAttributionKey>, AttributionResolutionError>
        {
            Err(AttributionResolutionError::Unavailable)
        }
    }

    fn attributed(key: &[u8; 32]) -> Wrap {
        let sender = Identity::from_seed([1; 32]);
        let recipient = Identity::from_seed([2; 32]);
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Us,
            [3; 32],
            AUGUST_2026,
            1,
        );
        envelope::wrap_attributed(
            &sender,
            &recipient.verifying_key(),
            "hello",
            NOW_SECS,
            key,
            &key_id,
        )
        .unwrap()
    }

    #[test]
    fn required_attribution_needs_a_configured_live_matching_key() {
        let key = [7; 32];
        let wrap = attributed(&key);
        let live: Arc<dyn AttributionKeyResolver> = Arc::new(FixedResolver {
            key: Some(ResolvedAttributionKey {
                public_key: key,
                not_before_ms: AUGUST_2026,
                not_after_ms: SEPTEMBER_2026,
                status: ComplianceKeyStatus::Active,
            }),
        });
        assert!(validate(
            &wrap,
            Some(&AttributionRequirement::new(Jurisdiction::Us, [3; 32])),
            &live,
            NOW_MS,
        )
        .is_ok());

        let unknown: Arc<dyn AttributionKeyResolver> = Arc::new(FixedResolver { key: None });
        assert!(matches!(
            validate(
                &wrap,
                Some(&AttributionRequirement::new(Jurisdiction::Us, [3; 32])),
                &unknown,
                NOW_MS,
            ),
            Err(LoftError::AttributionUnavailable)
        ));
        let unconfigured: Arc<dyn AttributionKeyResolver> =
            Arc::new(UnconfiguredAttributionResolver);
        assert!(matches!(
            validate(
                &wrap,
                Some(&AttributionRequirement::new(Jurisdiction::Us, [3; 32])),
                &unconfigured,
                NOW_MS,
            ),
            Err(LoftError::AttributionUnavailable)
        ));
        let unavailable: Arc<dyn AttributionKeyResolver> = Arc::new(UnavailableResolver);
        assert!(matches!(
            validate(
                &wrap,
                Some(&AttributionRequirement::new(Jurisdiction::Us, [3; 32])),
                &unavailable,
                NOW_MS,
            ),
            Err(LoftError::AttributionUnavailable)
        ));
    }

    #[test]
    fn configured_resolver_rejects_expired_and_digest_mismatched_blocks() {
        let wrap = attributed(&[7; 32]);
        let expired: Arc<dyn AttributionKeyResolver> = Arc::new(FixedResolver {
            key: Some(ResolvedAttributionKey {
                public_key: [7; 32],
                not_before_ms: AUGUST_2026,
                not_after_ms: SEPTEMBER_2026,
                status: ComplianceKeyStatus::Active,
            }),
        });
        assert!(matches!(
            validate(&wrap, None, &expired, SEPTEMBER_2026),
            Err(LoftError::AttributionRejected)
        ));

        let wrong: Arc<dyn AttributionKeyResolver> = Arc::new(FixedResolver {
            key: Some(ResolvedAttributionKey {
                public_key: [8; 32],
                not_before_ms: AUGUST_2026,
                not_after_ms: SEPTEMBER_2026,
                status: ComplianceKeyStatus::Active,
            }),
        });
        assert!(matches!(
            validate(&wrap, None, &wrong, NOW_MS),
            Err(LoftError::AttributionRejected)
        ));
    }

    #[test]
    fn retired_keys_and_sender_selected_scopes_are_rejected_for_new_admission() {
        let key = [7; 32];
        let wrap = attributed(&key);
        let retired: Arc<dyn AttributionKeyResolver> = Arc::new(FixedResolver {
            key: Some(ResolvedAttributionKey {
                public_key: key,
                not_before_ms: AUGUST_2026,
                not_after_ms: SEPTEMBER_2026,
                status: ComplianceKeyStatus::Retired,
            }),
        });
        let required = AttributionRequirement::new(Jurisdiction::Us, [3; 32]);
        assert!(matches!(
            validate(&wrap, Some(&required), &retired, NOW_MS),
            Err(LoftError::AttributionRejected)
        ));

        let active: Arc<dyn AttributionKeyResolver> = Arc::new(FixedResolver {
            key: Some(ResolvedAttributionKey {
                public_key: key,
                not_before_ms: AUGUST_2026,
                not_after_ms: SEPTEMBER_2026,
                status: ComplianceKeyStatus::Active,
            }),
        });
        let wrong_authority = AttributionRequirement::new(Jurisdiction::Us, [4; 32]);
        let wrong_jurisdiction = AttributionRequirement::new(Jurisdiction::Eu, [3; 32]);
        assert!(matches!(
            validate(&wrap, Some(&wrong_authority), &active, NOW_MS),
            Err(LoftError::AttributionRejected)
        ));
        assert!(matches!(
            validate(&wrap, Some(&wrong_jurisdiction), &active, NOW_MS),
            Err(LoftError::AttributionRejected)
        ));
    }
}
