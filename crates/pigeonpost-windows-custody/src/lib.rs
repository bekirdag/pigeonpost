//! Internal Windows handle-relative custody primitives.
//!
//! All native calls and raw-handle ownership transitions live in this crate so consumers can keep
//! `unsafe_code = "forbid"`. The public API accepts only borrowed safe objects and returns owned
//! `File` handles. Every successful directory handle omits delete sharing and carries a full
//! 128-bit Windows file identity.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{
    create_private_directory, file_identity, lock_directory, move_file_noclobber_write_through,
    open_directory, open_directory_for_child, replace_file_write_through, validate_component,
    CreateDirectory, FileIdentity, LockedDirectory,
};
