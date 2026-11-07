//! A cheap, cloneable view of bytes owned by a memory map or a heap buffer. Sections, FSTs and
//! record blocks are read through it in place.

use std::sync::Arc;

/// Owners must not move or change their bytes while alive (a `Vec` held in an `Arc`, a memory map,
/// an aligned buffer): the view keeps a pointer to them, so a read is a plain slice access.
#[derive(Clone)]
pub struct Bytes {
    owner: Arc<dyn AsRef<[u8]> + Send + Sync>,
    ptr: *const u8,
    len: usize,
    mapped: bool,
}

// SAFETY: `ptr` points into the immutable bytes of `owner`, which this view keeps alive; they are
// only read.
unsafe impl Send for Bytes {}
unsafe impl Sync for Bytes {}

impl Bytes {
    pub fn from_vec(v: Vec<u8>) -> Self {
        Self::from_owner(Arc::new(v))
    }

    /// Bytes in a heap buffer.
    pub fn from_owner(owner: Arc<dyn AsRef<[u8]> + Send + Sync>) -> Self {
        let data = (*owner).as_ref();
        let (ptr, len) = (data.as_ptr(), data.len());
        Self { owner, ptr, len, mapped: false }
    }

    /// Bytes of a memory-mapped file (page cache, not heap).
    pub fn from_map(owner: Arc<dyn AsRef<[u8]> + Send + Sync>) -> Self {
        Self { mapped: true, ..Self::from_owner(owner) }
    }

    /// Sub-range relative to this view. Panics when out of bounds (callers validate first).
    pub fn slice(&self, start: usize, len: usize) -> Self {
        assert!(start.checked_add(len).is_some_and(|end| end <= self.len), "Bytes::slice out of bounds");
        // SAFETY: start + len is within this view, checked above.
        let ptr = unsafe { self.ptr.add(start) };
        Self { owner: Arc::clone(&self.owner), ptr, len, mapped: self.mapped }
    }

    /// True when the bytes are a memory-mapped file rather than a heap buffer.
    #[inline]
    pub fn is_mapped(&self) -> bool {
        self.mapped
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn u32_at(&self, byte_offset: usize) -> u32 {
        u32::from_le_bytes(self[byte_offset..byte_offset + 4].try_into().unwrap())
    }

    #[inline]
    pub fn u64_at(&self, byte_offset: usize) -> u64 {
        u64::from_le_bytes(self[byte_offset..byte_offset + 8].try_into().unwrap())
    }
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        // SAFETY: `ptr..ptr + len` lies in the owner's bytes, alive and unchanged while `owner` is.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl AsRef<[u8]> for Bytes {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl std::fmt::Debug for Bytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bytes({} bytes)", self.len)
    }
}

struct PodVec<T>(Vec<T>);

impl<T: bytemuck::Pod> AsRef<[u8]> for PodVec<T> {
    fn as_ref(&self) -> &[u8] {
        bytemuck::cast_slice(&self.0)
    }
}

/// An array of plain numbers (`f32`, `u32`, ...) read in place from bytes aligned for them, or
/// held in a heap vector.
#[derive(Clone)]
pub struct Pods<T> {
    bytes: Bytes,
    _t: std::marker::PhantomData<T>,
}

impl<T: bytemuck::Pod + Send + Sync> Pods<T> {
    pub fn from_vec(v: Vec<T>) -> Self {
        Self { bytes: Bytes::from_owner(Arc::new(PodVec(v))), _t: std::marker::PhantomData }
    }

    /// View `bytes` as `T`s; bytes that arrive misaligned are copied once. `bytes.len()` must be a
    /// multiple of the element size.
    pub fn new(bytes: Bytes) -> std::io::Result<Self> {
        let size = std::mem::size_of::<T>();
        if bytes.len() % size != 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "array length is not a whole number of elements"));
        }
        if bytemuck::try_cast_slice::<u8, T>(&bytes).is_ok() {
            return Ok(Self { bytes, _t: std::marker::PhantomData });
        }
        let mut v = vec![T::zeroed(); bytes.len() / size];
        bytemuck::cast_slice_mut::<T, u8>(&mut v).copy_from_slice(&bytes);
        Ok(Self::from_vec(v))
    }

    /// The bytes of the array.
    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }
}

impl<T: bytemuck::Pod> std::ops::Deref for Pods<T> {
    type Target = [T];
    #[inline]
    fn deref(&self) -> &[T] {
        // Aligned and a whole number of elements: checked when the view was made.
        bytemuck::cast_slice(&self.bytes)
    }
}
