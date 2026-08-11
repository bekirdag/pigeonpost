//! Narrow boundary between request admission and the separately packaged sealed trace writer.
//!
//! This crate deliberately defines no sealing, key-custody, segment, or disclosure API. A trace
//! implementation receives one already-normalized request fact and must provide its own bounded
//! handoff. Raw network identifiers carried here must never be formatted into ordinary logs.

use std::any::Any;
use std::net::SocketAddr;

use pigeonpost_compliance_format::TraceRetentionPolicy;

/// Maximum number of blocking request tasks allowed to wait for trace durability concurrently.
///
/// This is large enough for the sealed sink to form a group-commit batch and small enough that a
/// stalled sink cannot create an unbounded Tokio blocking queue.
pub(crate) const TRACE_BLOCKING_LANES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceOperation {
    Publish {
        event_id: [u8; 32],
        recipient: [u8; 32],
        size_bytes: u32,
    },
    Fetch {
        owner: [u8; 32],
    },
    /// Mutation of signed public agent routing metadata: an agent or rotation record.
    PutAgent,
    Claim {
        correlation_id: [u8; 32],
    },
}

/// Sensitive request fact handed to the sealed writer. Do not derive `Display` or serialize this
/// into ordinary telemetry: the connected address and port are deliberately private evidence.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TraceInput {
    pub timestamp_ms: u64,
    pub connected_source: SocketAddr,
    pub operation: TraceOperation,
}

#[derive(Debug, thiserror::Error)]
pub enum TraceSinkError {
    #[error("trace sink unavailable")]
    Unavailable,
    #[error("trace storage budget exhausted")]
    Capacity,
}

/// Immutable append-only capacity plan advertised by an online trace sink.
///
/// This is an application-level admission contract. It does not assert a filesystem quota or
/// replace the operator's independently reviewed retention and custody controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceCapacity {
    pub policy: TraceRetentionPolicy,
    pub records_per_minute: u32,
    pub utc_epochs: u64,
    pub max_records_per_segment: u32,
    pub logical_limit_bytes: u64,
}

/// Synchronous, bounded cache/channel interface. `capture` may wait briefly for a bounded channel
/// but must return within the caller-enforced deadline. It must never silently drop a record.
pub trait TraceSink: Send + Sync + Any {
    fn enabled(&self) -> bool {
        true
    }

    fn readiness(&self) -> std::result::Result<(), TraceSinkError>;

    /// Return the immutable capacity plan enforced by this sink. Public serving refuses an
    /// enabled sink without a plan covering the listener's global admission ceiling.
    fn capacity_contract(&self) -> Option<TraceCapacity> {
        None
    }

    fn capture(&self, input: TraceInput) -> std::result::Result<(), TraceSinkError>;

    /// Durably close an active segment during coordinated server shutdown.
    fn shutdown(&self, _timestamp_ms: u64) -> std::result::Result<(), TraceSinkError> {
        Ok(())
    }
}

#[derive(Default)]
pub struct NoopTraceSink;

impl TraceSink for NoopTraceSink {
    fn enabled(&self) -> bool {
        false
    }

    fn readiness(&self) -> std::result::Result<(), TraceSinkError> {
        Ok(())
    }

    fn capture(&self, _input: TraceInput) -> std::result::Result<(), TraceSinkError> {
        Ok(())
    }
}
