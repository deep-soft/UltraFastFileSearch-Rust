use std::ffi::{OsStr, OsString};
use std::os::windows::prelude::OsStrExt;


// Convert Rust string to a wide string (Vec<u16>) with null termination.
pub(crate) fn to_wide_string_with_null(s: &OsStr) -> Vec<u16> {
    OsString::from(s).encode_wide().chain(Some(0)).collect()
}