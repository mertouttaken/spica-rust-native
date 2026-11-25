//! Shared FFI memory validation and aliasing rejection guard for JNI and C ABI boundaries.
//!
//! Enforces Rust slice preconditions (`slice::from_raw_parts` / `from_raw_parts_mut`) BEFORE
//! constructing any `&[T]` or `&mut [T]` reference:
//! - Non-null pointer (for `count > 0`)
//! - Strict alignment (`ptr as usize % align_of::<T>() == 0`)
//! - Overflow-safe byte size (`count * size_of::<T>() <= isize::MAX`)
//! - Overflow-safe address range (`addr.checked_add(byte_len)`)
//! - Aliasing rejection: no mutable span may overlap any other read or write span

use std::mem::{align_of, size_of};
use std::slice;

const MAX_TRACKED_SPANS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiGuardError {
    NullPointer,
    UnalignedPointer,
    SizeOverflow,
    CapacityTooSmall,
    AddressOverflow,
    AliasingViolation,
    TooManySpans,
    ReadOnlyBuffer,
    JniBufferError,
}

/// Validated raw immutable span ready to be converted into `&[T]` once all sibling spans pass.
#[derive(Debug, Clone, Copy)]
pub struct RawReadSpan<T> {
    ptr: *const T,
    len: usize,
}

impl<T> RawReadSpan<T> {
    pub const fn empty() -> Self {
        Self {
            ptr: std::ptr::NonNull::<T>::dangling().as_ptr(),
            len: 0,
        }
    }

    /// Converts the validated span into a Rust slice tied to `&self`'s lifetime (R1).
    ///
    /// # Safety
    /// Caller guarantees the backing memory remains valid, initialized for `T`, and unmutated
    /// for the borrow duration.
    #[inline]
    pub unsafe fn as_slice(&self) -> &[T] {
        if self.len == 0 {
            &[]
        } else {
            slice::from_raw_parts(self.ptr, self.len)
        }
    }
}

/// Validated raw mutable span ready to be converted into `&mut [T]` once all sibling spans pass.
/// Intentionally NOT `Copy`/`Clone` so a validated span can never yield two aliased `&mut [T]` slices.
#[derive(Debug)]
pub struct RawWriteSpan<T> {
    ptr: *mut T,
    len: usize,
}

impl<T> RawWriteSpan<T> {
    pub const fn empty() -> Self {
        Self {
            ptr: std::ptr::NonNull::<T>::dangling().as_ptr(),
            len: 0,
        }
    }

    /// Converts the validated span into a mutable Rust slice tied to `&mut self`'s lifetime,
    /// statically preventing double-aliasing from the same span handle.
    ///
    /// # Safety
    /// Caller guarantees exclusive ownership and validity of the backing memory for the borrow duration.
    #[inline]
    pub unsafe fn as_mut_slice(&mut self) -> &mut [T] {
        if self.len == 0 {
            &mut []
        } else {
            slice::from_raw_parts_mut(self.ptr, self.len)
        }
    }
}

/// Stack-allocated zero-heap interval validator tracking all read and write byte ranges for a
/// single FFI invocation.
#[derive(Debug)]
pub struct SpanValidator {
    reads: [(usize, usize); MAX_TRACKED_SPANS],
    read_count: usize,
    writes: [(usize, usize); MAX_TRACKED_SPANS],
    write_count: usize,
}

impl SpanValidator {
    #[inline]
    pub const fn new() -> Self {
        Self {
            reads: [(0, 0); MAX_TRACKED_SPANS],
            read_count: 0,
            writes: [(0, 0); MAX_TRACKED_SPANS],
            write_count: 0,
        }
    }

    #[inline]
    fn ranges_overlap(a: (usize, usize), b: (usize, usize)) -> bool {
        a.0 < b.1 && b.0 < a.1
    }

    fn compute_range<T>(
        addr: usize,
        count: usize,
        capacity_bytes: Option<usize>,
    ) -> Result<(usize, usize), FfiGuardError> {
        let needed_bytes = count
            .checked_mul(size_of::<T>())
            .ok_or(FfiGuardError::SizeOverflow)?;
        if needed_bytes > isize::MAX as usize {
            return Err(FfiGuardError::SizeOverflow);
        }
        if let Some(cap) = capacity_bytes {
            if cap < needed_bytes {
                return Err(FfiGuardError::CapacityTooSmall);
            }
        }
        if addr == 0 {
            return Err(FfiGuardError::NullPointer);
        }
        if addr % align_of::<T>() != 0 {
            return Err(FfiGuardError::UnalignedPointer);
        }
        let end = addr
            .checked_add(needed_bytes)
            .ok_or(FfiGuardError::AddressOverflow)?;
        Ok((addr, end))
    }

    /// Validates and registers a read-only raw pointer span.
    /// Rejects overlap against any previously registered write span.
    pub fn register_read<T>(
        &mut self,
        ptr: *const T,
        count: usize,
        capacity_bytes: Option<usize>,
    ) -> Result<RawReadSpan<T>, FfiGuardError> {
        if count == 0 {
            return Ok(RawReadSpan::empty());
        }
        let range = Self::compute_range::<T>(ptr as usize, count, capacity_bytes)?;
        for &w in &self.writes[..self.write_count] {
            if Self::ranges_overlap(range, w) {
                return Err(FfiGuardError::AliasingViolation);
            }
        }
        if self.read_count >= MAX_TRACKED_SPANS {
            return Err(FfiGuardError::TooManySpans);
        }
        self.reads[self.read_count] = range;
        self.read_count += 1;
        Ok(RawReadSpan { ptr, len: count })
    }

    /// Validates and registers a mutable raw pointer span.
    /// Rejects overlap against ALL previously registered read AND write spans.
    pub fn register_write<T>(
        &mut self,
        ptr: *mut T,
        count: usize,
        capacity_bytes: Option<usize>,
    ) -> Result<RawWriteSpan<T>, FfiGuardError> {
        if count == 0 {
            return Ok(RawWriteSpan::empty());
        }
        let range = Self::compute_range::<T>(ptr as usize, count, capacity_bytes)?;
        for &r in &self.reads[..self.read_count] {
            if Self::ranges_overlap(range, r) {
                return Err(FfiGuardError::AliasingViolation);
            }
        }
        for &w in &self.writes[..self.write_count] {
            if Self::ranges_overlap(range, w) {
                return Err(FfiGuardError::AliasingViolation);
            }
        }
        if self.write_count >= MAX_TRACKED_SPANS {
            return Err(FfiGuardError::TooManySpans);
        }
        self.writes[self.write_count] = range;
        self.write_count += 1;
        Ok(RawWriteSpan { ptr, len: count })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_read_aliasing_allowed_but_read_write_and_write_write_rejected() {
        let mut buf = [1.0f32; 16];

        // Read + Read on same memory is safe in Rust
        let mut v_rr = SpanValidator::new();
        assert!(v_rr.register_read(buf.as_ptr(), 16, Some(64)).is_ok());
        assert!(v_rr.register_read(buf.as_ptr(), 16, Some(64)).is_ok());

        // Read + Write overlapping must be rejected
        let mut v_rw = SpanValidator::new();
        assert!(v_rw.register_read(buf.as_ptr(), 8, Some(32)).is_ok());
        assert_eq!(
            v_rw.register_write(unsafe { buf.as_mut_ptr().add(4) }, 8, Some(32))
                .unwrap_err(),
            FfiGuardError::AliasingViolation
        );

        // Write + Write overlapping (even partial) must be rejected
        let mut v_ww = SpanValidator::new();
        assert!(v_ww.register_write(buf.as_mut_ptr(), 8, Some(32)).is_ok());
        assert_eq!(
            v_ww.register_write(unsafe { buf.as_mut_ptr().add(7) }, 4, Some(16))
                .unwrap_err(),
            FfiGuardError::AliasingViolation
        );

        // Adjacent non-overlapping writes are valid
        let mut v_adj = SpanValidator::new();
        assert!(v_adj.register_write(buf.as_mut_ptr(), 8, Some(32)).is_ok());
        assert!(v_adj
            .register_write(unsafe { buf.as_mut_ptr().add(8) }, 8, Some(32))
            .is_ok());
    }

    #[test]
    fn test_alignment_capacity_and_isize_max_enforced() {
        let bytes = [0u8; 32];
        let mut v = SpanValidator::new();

        // Force unaligned f32 pointer (offset +1 byte)
        let unaligned = unsafe { bytes.as_ptr().add(1) } as *const f32;
        assert_eq!(
            v.register_read(unaligned, 4, Some(16)).unwrap_err(),
            FfiGuardError::UnalignedPointer
        );

        // Capacity smaller than count * size_of::<f32>()
        let aligned = [0.0f32; 4];
        assert_eq!(
            v.register_read(aligned.as_ptr(), 4, Some(12)).unwrap_err(),
            FfiGuardError::CapacityTooSmall
        );

        // isize::MAX byte overflow
        assert_eq!(
            v.register_read(aligned.as_ptr(), (isize::MAX as usize / 4) + 1, None)
                .unwrap_err(),
            FfiGuardError::SizeOverflow
        );

        // Null pointer for non-zero count
        assert_eq!(
            v.register_read(std::ptr::null::<f32>(), 1, Some(4))
                .unwrap_err(),
            FfiGuardError::NullPointer
        );
    }
}
