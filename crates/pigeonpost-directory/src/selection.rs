//! Loft selection — the mechanism that takes weight off our shoulders.
//!
//! Three properties, each load-bearing (`docs/network.md`):
//!
//! - **Weighted-random, not best-first.** Deterministically picking "the best" loft stampedes
//!   whichever node looks best this hour, then the next one. Randomness spreads load with no
//!   coordination between clients.
//! - **Sticky.** An agent does not churn. Re-selection happens on failure, drain, or policy
//!   mismatch — never because a better option appeared. The consequence is worth stating: relief
//!   applies to *growth*, not to installed base, so our load ratchets down rather than dropping.
//! - **Diverse.** No two lofts in one list share an endpoint host or a declared operator label,
//!   because three relays in one rack is correlated failure wearing a disguise. A label is
//!   advisory and can only collapse more candidates; it never replaces the authenticated host.
//!
//! Our own lofts advertise a capacity equal to our budget, so as the pool grows our share of new
//! agents falls automatically — no migration, no client update, no decision by anyone.

use std::collections::HashSet;

use crate::entry::{parse_hex32, DirectoryEntry};

/// How many lofts an agent publishes in its loft list.
pub const TARGET_LOFTS: usize = 3;

/// What an agent needs from a loft before it will use one.
#[derive(Debug, Clone)]
pub struct SelectionCriteria {
    pub target: usize,
    pub min_retention_days: u64,
    pub min_event_bytes: usize,
}

impl Default for SelectionCriteria {
    fn default() -> Self {
        SelectionCriteria {
            target: TARGET_LOFTS,
            min_retention_days: 7,
            min_event_bytes: 32 * 1024,
        }
    }
}

/// Deterministic randomness, so selection is testable and reproducible from a seed.
pub struct Rng(u64);

impl Rng {
    pub fn seeded(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    pub fn from_entropy() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x2545F4914F6CDD1D);
        Rng(nanos.max(1))
    }

    /// xorshift64*, which is plenty for load spreading and needs no dependency.
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Choose a loft list.
///
/// `keep` is what the agent already uses and should not churn away from. `own` is the operator's
/// own loft, which always goes first when present — that is the recipient-hosted default that
/// makes the whole cost model work.
pub fn select<'a>(
    own: Option<&'a DirectoryEntry>,
    keep: &[&'a DirectoryEntry],
    pool: &'a [DirectoryEntry],
    criteria: &SelectionCriteria,
    rng: &mut Rng,
) -> Vec<&'a DirectoryEntry> {
    let mut chosen: Vec<&DirectoryEntry> = Vec::with_capacity(criteria.target);
    let mut failure_domains = HashSet::new();

    let push = |entry: &'a DirectoryEntry,
                chosen: &mut Vec<&'a DirectoryEntry>,
                failure_domains: &mut HashSet<String>| {
        chosen.push(entry);
        failure_domains.extend(entry.failure_domains());
    };

    let conflicts = |entry: &DirectoryEntry, failure_domains: &HashSet<String>| {
        entry
            .failure_domains()
            .iter()
            .any(|domain| failure_domains.contains(domain))
    };

    if let Some(own) = own {
        push(own, &mut chosen, &mut failure_domains);
    }

    // Stickiness: anything still usable stays, even if something better showed up.
    for entry in keep {
        if chosen.len() >= criteria.target {
            break;
        }
        if conflicts(entry, &failure_domains) {
            continue;
        }
        if matches!(
            entry.state,
            crate::entry::LoftState::Active | crate::entry::LoftState::Draining
        ) {
            push(entry, &mut chosen, &mut failure_domains);
        }
    }

    let mut candidates: Vec<&DirectoryEntry> = pool
        .iter()
        .filter(|e| e.state.selectable())
        .filter(|e| e.retention_days >= criteria.min_retention_days)
        .filter(|e| e.policy.max_event_bytes >= criteria.min_event_bytes)
        .filter(|e| e.weight() > 0.0)
        .collect();

    while chosen.len() < criteria.target && !candidates.is_empty() {
        candidates.retain(|entry| !conflicts(entry, &failure_domains));
        if candidates.is_empty() {
            break;
        }

        let total: f64 = candidates.iter().map(|e| e.weight()).sum();
        if total <= 0.0 {
            break;
        }

        let mut point = rng.next_f64() * total;
        let mut picked = candidates.len() - 1;
        for (i, entry) in candidates.iter().enumerate() {
            point -= entry.weight();
            if point <= 0.0 {
                picked = i;
                break;
            }
        }

        let entry = candidates.remove(picked);
        push(entry, &mut chosen, &mut failure_domains);
    }

    chosen
}

/// Rendezvous lofts for an address: the deterministic, failure-domain-diverse set that both the
/// publisher and any sender compute independently.
///
/// This is what closes the bootstrap loop — an agent record lists the lofts, but you need a loft
/// to fetch the record. Ranking by `SHA-256(loft_pubkey ‖ address)` means a sender who knows only
/// the address knows exactly where to ask (`docs/sds.md` §5.2). The signed endpoint host is an
/// authenticated failure domain; an operator label may collapse additional entries but is only
/// advisory, so deployments still need independent admission/Sybil policy for operator diversity.
pub fn rendezvous<'a>(
    pool: &'a [DirectoryEntry],
    address: &str,
    count: usize,
) -> Vec<&'a DirectoryEntry> {
    use sha2::{Digest, Sha256};

    let mut ranked: Vec<([u8; 32], &DirectoryEntry)> = pool
        .iter()
        .filter(|e| e.state.selectable())
        .filter_map(|entry| {
            let pubkey = parse_hex32(&entry.pubkey)?;
            let mut hasher = Sha256::new();
            hasher.update(pubkey);
            hasher.update(address.as_bytes());
            Some((hasher.finalize().into(), entry))
        })
        .collect();

    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.pubkey.cmp(&b.1.pubkey))
            .then_with(|| a.1.endpoint.cmp(&b.1.endpoint))
    });
    let mut selected = Vec::with_capacity(count);
    let mut failure_domains = HashSet::new();
    for (_, entry) in ranked {
        if selected.len() >= count {
            break;
        }
        let domains = entry.failure_domains();
        if domains
            .iter()
            .any(|domain| failure_domains.contains(domain))
        {
            continue;
        }
        failure_domains.extend(domains);
        selected.push(entry);
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Health, LoftPolicy, LoftState};
    use ed25519_dalek::SigningKey;

    fn entry(seed: u8, endpoint: &str, capacity_gb: u64, operator: Option<&str>) -> DirectoryEntry {
        let mut e = DirectoryEntry::signed(
            &SigningKey::from_bytes(&[seed; 32]),
            endpoint,
            operator.map(str::to_string),
            capacity_gb,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65536,
            },
            0.0,
        );
        e.state = LoftState::Active;
        e.health = Health {
            uptime_30d: 1.0,
            probe_fail_streak: 0,
            last_probe: 1,
        };
        e
    }

    fn pool() -> Vec<DirectoryEntry> {
        vec![
            entry(1, "wss://a.example", 100, Some("/github/a")),
            entry(2, "wss://b.example", 100, Some("/github/b")),
            entry(3, "wss://c.example", 100, Some("/github/c")),
            entry(4, "wss://d.example", 100, Some("/github/d")),
            entry(5, "wss://e.example", 100, Some("/github/e")),
        ]
    }

    #[test]
    fn selects_the_target_number() {
        let pool = pool();
        let chosen = select(
            None,
            &[],
            &pool,
            &SelectionCriteria::default(),
            &mut Rng::seeded(1),
        );
        assert_eq!(chosen.len(), TARGET_LOFTS);
    }

    #[test]
    fn an_own_loft_always_comes_first() {
        // Recipient-hosted by default: the operator's own box carries their agents' mail.
        let pool = pool();
        let own = entry(9, "wss://mine.example", 1, Some("/github/me"));
        let chosen = select(
            Some(&own),
            &[],
            &pool,
            &SelectionCriteria::default(),
            &mut Rng::seeded(1),
        );

        assert_eq!(chosen[0].endpoint, "wss://mine.example");
        assert_eq!(chosen.len(), TARGET_LOFTS);
    }

    #[test]
    fn no_two_lofts_share_an_operator() {
        let pool = vec![
            entry(1, "wss://a1.example", 100, Some("/github/same")),
            entry(2, "wss://a2.example", 100, Some("/github/same")),
            entry(3, "wss://a3.example", 100, Some("/github/same")),
            entry(4, "wss://b.example", 100, Some("/github/other")),
        ];
        let chosen = select(
            None,
            &[],
            &pool,
            &SelectionCriteria::default(),
            &mut Rng::seeded(7),
        );

        assert_eq!(chosen.len(), 2, "only two distinct operators exist");
    }

    #[test]
    fn labels_cannot_split_one_host_into_multiple_failure_domains() {
        let pool = vec![
            entry(1, "wss://box.example:7717", 100, Some("/github/a")),
            entry(2, "wss://box.example:7718", 100, Some("/github/b")),
            entry(3, "wss://other.example", 100, Some("/github/c")),
        ];
        let chosen = select(
            None,
            &[],
            &pool,
            &SelectionCriteria::default(),
            &mut Rng::seeded(9),
        );

        assert_eq!(chosen.len(), 2);
        assert_eq!(
            chosen
                .iter()
                .filter(|entry| entry.endpoint.contains("box.example"))
                .count(),
            1
        );
    }

    #[test]
    fn existing_lofts_are_kept_rather_than_churned() {
        let pool = pool();
        let keep = vec![&pool[4]];
        let chosen = select(
            None,
            &keep,
            &pool,
            &SelectionCriteria::default(),
            &mut Rng::seeded(3),
        );

        assert!(
            chosen.iter().any(|e| e.endpoint == "wss://e.example"),
            "an agent must not churn away from a working loft"
        );
    }

    #[test]
    fn a_degraded_loft_is_not_selected_but_a_draining_one_is_kept() {
        let mut pool = pool();
        pool[0].state = LoftState::Degraded;
        pool[1].state = LoftState::Draining;

        let chosen = select(
            None,
            &[],
            &pool,
            &SelectionCriteria::default(),
            &mut Rng::seeded(5),
        );
        assert!(!chosen.iter().any(|e| e.endpoint == "wss://a.example"));
        assert!(!chosen.iter().any(|e| e.endpoint == "wss://b.example"));

        // But an agent already on the draining loft keeps it until the drain date.
        let draining = pool[1].clone();
        let kept = select(
            None,
            &[&draining],
            &pool,
            &SelectionCriteria::default(),
            &mut Rng::seeded(5),
        );
        assert!(kept.iter().any(|e| e.endpoint == "wss://b.example"));
    }

    #[test]
    fn lofts_that_cannot_meet_the_agents_needs_are_filtered_out() {
        let mut pool = pool();
        pool[0].retention_days = 1;
        pool[1].policy.max_event_bytes = 1024;

        let chosen = select(
            None,
            &[],
            &pool,
            &SelectionCriteria::default(),
            &mut Rng::seeded(11),
        );
        assert!(!chosen.iter().any(|e| e.endpoint == "wss://a.example"));
        assert!(!chosen.iter().any(|e| e.endpoint == "wss://b.example"));
    }

    #[test]
    fn capacity_weighting_spreads_load_in_proportion() {
        // The mechanism the whole cost model rests on: a loft advertising a tenth of the pool's
        // capacity should receive roughly a tenth of new agents, not a third.
        let pool = vec![
            entry(1, "wss://big.example", 900, Some("/github/big")),
            entry(2, "wss://small.example", 100, Some("/github/small")),
        ];

        let mut big = 0;
        let mut small = 0;
        for seed in 1..2001u64 {
            let chosen = select(
                None,
                &[],
                &pool,
                &SelectionCriteria {
                    target: 1,
                    ..Default::default()
                },
                &mut Rng::seeded(seed),
            );
            match chosen[0].endpoint.as_str() {
                "wss://big.example" => big += 1,
                _ => small += 1,
            }
        }

        let share = small as f64 / (big + small) as f64;
        assert!(
            (0.05..0.15).contains(&share),
            "expected ~10% to the small loft, got {share:.3}"
        );
    }

    #[test]
    fn our_share_falls_as_the_pool_grows() {
        // Our loft advertises a fixed budget. As others join, our share of new agents drops with
        // no migration and no client update — this is the dial from docs/capacity.md.
        let ours = entry(1, "wss://ours.example", 100, Some("/github/us"));

        let share_of = |pool: &[DirectoryEntry]| {
            let mut ours_picked = 0;
            for seed in 1..1001u64 {
                let chosen = select(
                    None,
                    &[],
                    pool,
                    &SelectionCriteria {
                        target: 1,
                        ..Default::default()
                    },
                    &mut Rng::seeded(seed),
                );
                if chosen[0].endpoint == "wss://ours.example" {
                    ours_picked += 1;
                }
            }
            ours_picked as f64 / 1000.0
        };

        let alone = vec![ours.clone()];
        let mut crowded = vec![ours.clone()];
        for i in 2..12u8 {
            crowded.push(entry(
                i,
                &format!("wss://n{i}.example"),
                100,
                Some(&format!("/github/n{i}")),
            ));
        }

        assert_eq!(share_of(&alone), 1.0);
        let crowded_share = share_of(&crowded);
        assert!(
            crowded_share < 0.2,
            "our share should fall toward 1/11, got {crowded_share:.3}"
        );
    }

    #[test]
    fn selection_is_reproducible_from_a_seed() {
        let pool = pool();
        let a = select(
            None,
            &[],
            &pool,
            &SelectionCriteria::default(),
            &mut Rng::seeded(42),
        );
        let b = select(
            None,
            &[],
            &pool,
            &SelectionCriteria::default(),
            &mut Rng::seeded(42),
        );
        assert_eq!(
            a.iter().map(|e| &e.endpoint).collect::<Vec<_>>(),
            b.iter().map(|e| &e.endpoint).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rendezvous_is_deterministic_and_agreed_by_both_sides() {
        let pool = pool();
        let address = "/k/6htgz65xb7yfs53dmhdanfmk7c";

        let publisher = rendezvous(&pool, address, 3);
        let sender = rendezvous(&pool, address, 3);

        assert_eq!(publisher.len(), 3);
        assert_eq!(
            publisher.iter().map(|e| &e.endpoint).collect::<Vec<_>>(),
            sender.iter().map(|e| &e.endpoint).collect::<Vec<_>>(),
            "publisher and sender must compute the same set or nothing is findable"
        );
    }

    #[test]
    fn rendezvous_never_treats_one_failure_domain_as_independent_lofts() {
        let pool = vec![
            entry(1, "wss://same.example:7001", 100, Some("operator-a")),
            entry(2, "wss://same.example:7002", 100, Some("operator-b")),
            entry(3, "wss://other.example", 100, Some("operator-a")),
            entry(4, "wss://third.example", 100, Some("operator-c")),
            entry(5, "wss://fourth.example", 100, Some("operator-d")),
        ];

        let selected = rendezvous(&pool, "/k/diverse", 4);
        let mut domains = HashSet::new();
        for loft in &selected {
            for domain in loft.failure_domains() {
                assert!(
                    domains.insert(domain.clone()),
                    "rendezvous reused failure domain {domain}"
                );
            }
        }
        assert!(selected.len() >= 3);
        assert_eq!(
            rendezvous(&pool[..2], "/k/same-host-only", 3).len(),
            1,
            "ports and keys on one host are not independent rendezvous targets"
        );
    }

    #[test]
    fn rendezvous_spreads_addresses_across_the_pool() {
        // If every address hashed to the same lofts, three nodes would carry every record.
        let pool = pool();
        let mut first_choice = std::collections::HashMap::new();
        for i in 0..200 {
            let address = format!("/k/address{i:04}");
            let chosen = rendezvous(&pool, &address, 1);
            *first_choice.entry(chosen[0].endpoint.clone()).or_insert(0) += 1;
        }
        assert_eq!(
            first_choice.len(),
            pool.len(),
            "every loft should see traffic"
        );
    }

    #[test]
    fn rendezvous_skips_lofts_that_are_not_active() {
        let mut pool = pool();
        for entry in pool.iter_mut() {
            entry.state = LoftState::Degraded;
        }
        assert!(rendezvous(&pool, "/k/anything", 3).is_empty());
    }
}
