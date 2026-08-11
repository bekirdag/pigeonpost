//! Audited boundary for macOS extended-ACL FFI.

#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::ptr;

type Acl = *mut c_void;
type AclEntry = *mut c_void;

const ACL_TYPE_EXTENDED: i32 = 0x0000_0100;
const ACL_FIRST_ENTRY: i32 = 0;
const ACL_NEXT_ENTRY: i32 = -1;
const ACL_EXTENDED_ALLOW: i32 = 1;
const ACL_EXTENDED_DENY: i32 = 2;

unsafe extern "C" {
    fn acl_get_fd_np(fd: i32, acl_type: i32) -> Acl;
    fn acl_get_entry(acl: Acl, entry_id: i32, entry: *mut AclEntry) -> i32;
    fn acl_get_tag_type(entry: AclEntry, tag_type: *mut i32) -> i32;
    fn acl_valid(acl: Acl) -> i32;
    fn acl_free(object: *mut c_void) -> i32;
}

struct OwnedAcl(Acl);

impl Drop for OwnedAcl {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a non-null allocation returned by `acl_get_fd_np`, is owned by this
        // guard, and is freed exactly once here.
        let _ = unsafe { acl_free(self.0) };
    }
}

/// Rejects any extended ALLOW entry while permitting an empty ACL or deny-only entries.
pub(super) fn validate_no_extended_allow(file: &File) -> io::Result<()> {
    // SAFETY: The borrowed raw fd remains valid for the duration of this call. The returned ACL,
    // when non-null, is transferred to `OwnedAcl` immediately.
    let raw_acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if raw_acl.is_null() {
        let error = io::Error::last_os_error();
        // macOS reports ENOENT when the object has no extended ACL at all. That is the safe,
        // empty-ACL case; every other retrieval failure remains fail-closed.
        return if error.raw_os_error() == Some(2) {
            Ok(())
        } else {
            Err(error)
        };
    }
    let acl = OwnedAcl(raw_acl);
    // SAFETY: `acl.0` is the live ACL returned by `acl_get_fd_np` and is not mutated here.
    if unsafe { acl_valid(acl.0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut entry: AclEntry = ptr::null_mut();
    let mut selector = ACL_FIRST_ENTRY;

    loop {
        // SAFETY: `acl.0` is live and owned by `acl`; `entry` is a valid out-pointer.
        // Darwin's `acl_get_entry` returns zero for an entry and -1/EINVAL after the final entry
        // (unlike the 1/0 convention used by several other ACL implementations).
        if unsafe { acl_get_entry(acl.0, selector, &mut entry) } != 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(22) {
                Ok(())
            } else {
                Err(error)
            };
        }
        if entry.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ACL iterator returned a null entry",
            ));
        }
        selector = ACL_NEXT_ENTRY;

        let mut tag = 0;
        // SAFETY: A successful `acl_get_entry` initialized `entry`; `tag` is a valid out-pointer.
        if unsafe { acl_get_tag_type(entry, &mut tag) } != 0 {
            return Err(io::Error::last_os_error());
        }
        match tag {
            ACL_EXTENDED_DENY => {}
            ACL_EXTENDED_ALLOW => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "extended ACL grants additional access",
                ));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown extended ACL entry type",
                ));
            }
        }
    }
}
