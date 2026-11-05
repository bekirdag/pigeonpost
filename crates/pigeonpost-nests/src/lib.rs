//! Pigeonpost Nests, Phase A (docs/nests/phase-a-plan.md).
//!
//! A Nest is one Pigeonpost agent mailbox with its own private, persistent workspace, running in a
//! sandbox on a dedicated worker host. This crate holds the three pieces that run there:
//!
//! - [`control`]: the Nest control service. Creates, starts, pauses, resumes, stops and deletes
//!   Nests; keeps the durable run, lease and usage ledger ([`db`]); stops idle Nests; and
//!   reconciles the host against the ledger after a restart. Only the postbox may call it, over a
//!   private WireGuard address, with a service token.
//! - [`broker`]: the model broker. The only network service a sandbox can reach on the host. It
//!   authenticates each Nest by a per-start token, reserves that Nest's token budget before every
//!   request, and calls the upstream model gateway (mswarm's Pigeonpost tenant) with a credential
//!   that never enters the sandbox.
//! - the runner binary (`pigeonpost-nest-runner`), which is the sandbox's entrypoint: it enrols the
//!   Nest's mailbox once, then runs agentd with the `nest` runtime.
//!
//! The sandbox runtime ([`sandbox`]) and the volume backend ([`storage`]) are traits. Phase A runs
//! gVisor (`runsc`, systrap platform) under podman, on loop-backed LVM thin volumes with per-Nest
//! LUKS2 keys, because this host has no KVM. A Firecracker/KVM implementation replaces
//! [`sandbox::GvisorPodman`] on future hardware without changing the control plane or the ledger.

pub mod broker;
pub mod config;
pub mod control;
pub mod db;
pub mod files;
pub mod mock_upstream;
pub mod parse;
pub mod sandbox;
pub mod storage;
pub mod util;
