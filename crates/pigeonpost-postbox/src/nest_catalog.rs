//! The Nest package catalog (Nests Phase B1; build plan §7.1, hosted-agents plan §9.1/§9.5).
//!
//! One versioned table of first-party Nest packages. Every rail maps onto it:
//!
//! - App Store: one auto-renewable monthly product per package in the **Nests** subscription group
//!   (separate from the handle group);
//! - Google Play: one subscription per package with the base plan `monthly`;
//! - web (iyzico Subscription V2 through the billing bridge): plan codes
//!   `nest-<kind>-<tier>-monthly-<usd|eur|try>`.
//!
//! The catalog decides what a verified purchase is worth (Nest slots, storage, model allowance).
//! A client never says what it bought: the postbox maps a provider-verified product id or a
//! bridge-asserted plan code through this table, and an unknown product grants nothing.
//!
//! Prices are **not** here. Store prices live in App Store Connect and Play Console (localized by
//! the store, read by the client from StoreKit/Billing), web prices in the billing bridge's
//! catalog. All of them are proposals pending owner approval (build plan §12, OD-16).

use serde::Serialize;

/// The catalog edition every entitlement written from it records as `plan_version`.
pub const CATALOG_VERSION: &str = "nests-2026-11";
/// Google Play base plan id shared by every Nest subscription.
pub const GOOGLE_BASE_PLAN: &str = "monthly";

const GIB: i64 = 1024 * 1024 * 1024;

/// Whether a package includes managed AI through the model broker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAccess {
    /// Pigeonpost AI: the broker's managed route, with the package's token allowance.
    Managed,
    /// Hosting only: no model is connected; the Nest makes no paid model call.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Package {
    /// Logical package code, also the plan code of store-mirrored entitlements.
    pub code: &'static str,
    /// Display name (English; clients localize from `code`).
    pub name: &'static str,
    /// Higher is more capacity. Upgrades go up, downgrades down; Apple subscription levels follow
    /// it (highest rank = level 1).
    pub rank: u8,
    pub slots: i64,
    /// Pooled hosted-file storage for the package, in bytes.
    pub storage_bytes: i64,
    /// Running-workspace hours per month, pooled (shown; metered from B5).
    pub active_hours: i64,
    /// Monthly managed-AI allowance, pooled across the package's Nests.
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub model: ModelAccess,
    pub apple_product: &'static str,
    pub google_product: &'static str,
    /// Web plan code stem; the bridge appends `-usd`, `-eur` or `-try`.
    pub web_plan: &'static str,
    /// Offered for new purchases. A package that does not fit the current Nests host stays in the
    /// table (so a future purchase maps) but is not offered.
    pub sellable: bool,
}

impl Package {
    /// Per-Nest share of the pooled model allowance. The broker enforces budgets per Nest today,
    /// so a pooled package is split evenly rather than handed out once per Nest (which would
    /// multiply it by the slot count).
    pub fn per_nest_tokens(&self) -> (i64, i64) {
        let slots = self.slots.max(1);
        (self.input_tokens / slots, self.output_tokens / slots)
    }

    /// Per-Nest volume quota: the pooled storage split evenly, at least 1 GiB.
    pub fn per_nest_storage_bytes(&self) -> i64 {
        (self.storage_bytes / self.slots.max(1)).max(GIB)
    }
}

/// The catalog. Order is display order.
pub const PACKAGES: &[Package] = &[
    Package {
        code: "nest.ai.starter.monthly",
        name: "Pigeonpost AI",
        rank: 2,
        slots: 1,
        storage_bytes: 5 * GIB,
        active_hours: 10,
        input_tokens: 1_000_000,
        output_tokens: 100_000,
        model: ModelAccess::Managed,
        apple_product: "dev.pigeonpost.inbox.nests.ai.starter.monthly",
        google_product: "pigeonpost.nests.ai.starter",
        web_plan: "nest-ai-starter-monthly",
        sellable: true,
    },
    Package {
        code: "nest.ai.plus.monthly",
        name: "Pigeonpost AI Plus",
        rank: 3,
        slots: 3,
        storage_bytes: 20 * GIB,
        active_hours: 40,
        input_tokens: 5_000_000,
        output_tokens: 500_000,
        model: ModelAccess::Managed,
        apple_product: "dev.pigeonpost.inbox.nests.ai.plus.monthly",
        google_product: "pigeonpost.nests.ai.plus",
        web_plan: "nest-ai-plus-monthly",
        sellable: true,
    },
    Package {
        code: "nest.ai.pro.monthly",
        name: "Pigeonpost AI Pro",
        rank: 4,
        slots: 10,
        storage_bytes: 100 * GIB,
        active_hours: 120,
        input_tokens: 15_000_000,
        output_tokens: 1_500_000,
        model: ModelAccess::Managed,
        apple_product: "dev.pigeonpost.inbox.nests.ai.pro.monthly",
        google_product: "pigeonpost.nests.ai.pro",
        web_plan: "nest-ai-pro-monthly",
        // 100 GiB pooled does not fit nests-1's 62 GiB thin pool: not offered until a second host.
        sellable: false,
    },
    Package {
        code: "nest.host.starter.monthly",
        name: "Empty Nest",
        rank: 1,
        slots: 1,
        storage_bytes: 5 * GIB,
        active_hours: 10,
        input_tokens: 0,
        output_tokens: 0,
        model: ModelAccess::None,
        apple_product: "dev.pigeonpost.inbox.nests.host.starter.monthly",
        google_product: "pigeonpost.nests.host.starter",
        web_plan: "nest-host-starter-monthly",
        sellable: true,
    },
];

pub fn by_code(code: &str) -> Option<&'static Package> {
    PACKAGES.iter().find(|p| p.code == code)
}

pub fn by_apple_product(product: &str) -> Option<&'static Package> {
    PACKAGES.iter().find(|p| p.apple_product == product)
}

pub fn by_google_product(product: &str) -> Option<&'static Package> {
    PACKAGES.iter().find(|p| p.google_product == product)
}

pub fn apple_products() -> Vec<String> {
    PACKAGES
        .iter()
        .map(|p| p.apple_product.to_string())
        .collect()
}

pub fn google_products() -> Vec<String> {
    PACKAGES
        .iter()
        .map(|p| p.google_product.to_string())
        .collect()
}

/// The package an entitlement's plan code stands for: a store-mirrored `nest.*` code exactly, or
/// a web `nest-…-monthly-<usd|eur|try>` code. Anything else is not a Nest package.
pub fn for_plan_code(code: &str) -> Option<&'static Package> {
    if let Some(p) = by_code(code) {
        return Some(p);
    }
    let (stem, currency) = code.rsplit_once('-')?;
    if !matches!(currency, "usd" | "eur" | "try") {
        return None;
    }
    PACKAGES.iter().find(|p| p.web_plan == stem)
}

/// Is this plan code in the Nest namespace at all (known or not)? Such codes never count as web
/// name capacity, and the internal API refuses an unknown one instead of storing it.
pub fn is_nest_plan_code(code: &str) -> bool {
    code.starts_with("nest.") || code.starts_with("nest-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_product_ids_are_unique_and_map_back() {
        let mut seen = std::collections::HashSet::new();
        for p in PACKAGES {
            for id in [p.code, p.apple_product, p.google_product, p.web_plan] {
                assert!(seen.insert(id), "duplicate id {id}");
            }
            assert_eq!(by_code(p.code), Some(p));
            assert_eq!(by_apple_product(p.apple_product), Some(p));
            assert_eq!(by_google_product(p.google_product), Some(p));
            for cur in ["usd", "eur", "try"] {
                assert_eq!(for_plan_code(&format!("{}-{cur}", p.web_plan)), Some(p));
            }
            assert!(is_nest_plan_code(p.code));
            assert!(is_nest_plan_code(p.web_plan));
            assert!(crate::store::entitlements::plan_code_for(p.apple_product).len() <= 64);
        }
        let ranks: std::collections::HashSet<_> = PACKAGES.iter().map(|p| p.rank).collect();
        assert_eq!(
            ranks.len(),
            PACKAGES.len(),
            "ranks order upgrades and must be distinct"
        );
    }

    #[test]
    fn unknown_or_foreign_codes_are_not_packages() {
        for code in [
            "",
            "nest",
            "nest.ai.starter",
            "nest-ai-starter-monthly",
            "nest-ai-starter-monthly-gbp",
            "nest-ai-starter-yearly-usd",
            "cloud-1names-yearly-usd",
            "ent-5-monthly-usd",
            "dev.pigeonpost.inbox.nests.ai.starter.monthly",
            "NEST.AI.STARTER.MONTHLY",
        ] {
            assert!(for_plan_code(code).is_none(), "{code}");
        }
        assert!(!is_nest_plan_code("cloud-1names-yearly-usd"));
        assert!(!is_nest_plan_code("nestle-1"));
    }

    #[test]
    fn allowances_are_split_never_multiplied() {
        let plus = by_code("nest.ai.plus.monthly").unwrap();
        let (i, o) = plus.per_nest_tokens();
        assert!(i * plus.slots <= plus.input_tokens && o * plus.slots <= plus.output_tokens);
        let host = by_code("nest.host.starter.monthly").unwrap();
        assert_eq!(host.model, ModelAccess::None);
        assert_eq!(host.per_nest_tokens(), (0, 0));
        for p in PACKAGES {
            assert!(p.per_nest_storage_bytes() * p.slots <= p.storage_bytes.max(GIB * p.slots));
        }
    }
}
