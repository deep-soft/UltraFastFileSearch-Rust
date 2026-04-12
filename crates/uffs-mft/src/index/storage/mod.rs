//! Persistent on-disk serialization for `MftIndex` snapshots.

/// Binary index deserialization implementation.
mod deserialize;
/// File-based storage wrappers.
mod file_io;
/// Storage header and format version metadata.
mod header;
/// Binary index serialization implementation.
mod serialize;

pub use self::header::IndexHeader;

/// Alignment-safe bulk copy from a `&[u8]` slice into a properly aligned
/// `Vec<T>`.
///
/// Unlike `bytemuck::cast_slice`, this works regardless of the source
/// pointer's alignment. It allocates a `Vec<T>` (which the allocator
/// guarantees to be `align_of::<T>()`-aligned), then copies the raw bytes
/// in via `copy_from_slice`.
///
/// # Panics
///
/// Panics if `bytes.len()` is not an exact multiple of `size_of::<T>()`.
pub(crate) fn aligned_vec_from_bytes<T: bytemuck::Pod>(bytes: &[u8]) -> Vec<T> {
    let elem_size = size_of::<T>();
    assert!(
        elem_size > 0 && bytes.len().is_multiple_of(elem_size),
        "byte slice length {} is not a multiple of element size {}",
        bytes.len(),
        elem_size,
    );
    let count = bytes.len() / elem_size;
    let mut vec = vec![T::zeroed(); count];
    let dst = bytemuck::cast_slice_mut::<T, u8>(&mut vec);
    dst.copy_from_slice(bytes);
    vec
}
