//! Runtime boundary for offline compliance operations shipped only on Linux and macOS.

use crate::error::{ComplianceError, Result};

/// Opaque capability token. No target outside Linux and macOS can construct a supported token.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OfflinePlatform {
    supported_offline_target: bool,
}

impl OfflinePlatform {
    pub(crate) const fn current() -> Self {
        Self {
            supported_offline_target: cfg!(any(target_os = "linux", target_os = "macos")),
        }
    }

    #[cfg(test)]
    pub(crate) const fn unsupported_for_test() -> Self {
        Self {
            supported_offline_target: false,
        }
    }

    /// Refuse an operational entry point before it can inspect inputs or cause effects.
    pub(crate) fn require(self) -> Result<()> {
        if self.supported_offline_target {
            Ok(())
        } else {
            Err(ComplianceError::UnsupportedPlatform)
        }
    }
}
