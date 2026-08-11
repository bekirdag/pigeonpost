//! Internal descriptor-relative filesystem custody primitives for Unix.
//!
//! The public surface is intentionally absent on non-Unix targets. Consumers get retained file
//! descriptors and policy-checked operations without importing native APIs or writing `unsafe`.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(all(unix, target_os = "macos"))]
mod macos_acl;
#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use unix::{
    AncestorPolicy, CustodyError, DirPolicy, DirectoryEntries, DirectoryEntry, EntryKind,
    EntryMetadata, FilePolicy, GuardedDir, GuardedFile, LeafName, NormalizedPath, ObjectIdentity,
    OpenAccess, Result,
};
