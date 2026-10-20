use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use thiserror::Error;

use crate::{AssetCacheKey, ConvexDecompositionError, ConvexPart};

const ENTRY_MAGIC: &[u8; 8] = b"BOPCVX01";
const CHECKSUM_BYTES: usize = 32;
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PARTS: u32 = 1_024;
static NEXT_TEMPORARY_FILE: AtomicU64 = AtomicU64::new(0);

/// Whether convex parts came from persistent storage or fresh decomposition.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CacheDisposition {
    /// A validated entry was loaded without running the generator.
    Hit,
    /// No valid entry existed, so convex parts were generated and published.
    Miss,
}

/// Successful convex-cache result with stable ordered collision geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct CachedConvexParts {
    key: AssetCacheKey,
    disposition: CacheDisposition,
    parts: Vec<ConvexPart>,
}

impl CachedConvexParts {
    /// Returns the content key used for this lookup.
    #[must_use]
    pub const fn key(&self) -> AssetCacheKey {
        self.key
    }

    /// Reports whether this lookup was a hit or a generated miss.
    #[must_use]
    pub const fn disposition(&self) -> CacheDisposition {
        self.disposition
    }

    /// Returns collision parts in stable decomposition order.
    #[must_use]
    pub fn parts(&self) -> &[ConvexPart] {
        &self.parts
    }

    /// Consumes the result and returns its collision parts.
    #[must_use]
    pub fn into_parts(self) -> Vec<ConvexPart> {
        self.parts
    }
}

/// Filesystem operation that prevented a cache lookup or publication.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ConvexCacheIoOperation {
    /// Opening, sizing, or reading an existing entry.
    Read,
    /// Creating the content-addressed parent directory.
    CreateDirectory,
    /// Creating a unique same-directory temporary file.
    CreateTemporary,
    /// Writing the complete temporary entry.
    WriteTemporary,
    /// Removing a corrupt target before a platform-specific publication retry.
    RemoveCorrupt,
    /// Atomically renaming a completed temporary entry into place.
    Publish,
}

/// Error returned when convex-cache generation or filesystem access fails.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConvexDecompositionCacheError {
    /// Fresh V-HACD generation failed on a cache miss.
    #[error(transparent)]
    Decomposition(#[from] ConvexDecompositionError),
    /// A generator violated its contract by returning no collision parts.
    #[error("convex cache generator returned no collision parts")]
    EmptyGeneratedParts,
    /// Generated geometry exceeded the format's bounded part count.
    #[error("convex cache entry has {actual} parts, exceeding maximum {maximum}")]
    PartLimitExceeded {
        /// Number of generated convex parts.
        actual: usize,
        /// Largest supported number of convex parts.
        maximum: u32,
    },
    /// Generated geometry exceeded the compact entry's u32 count domain.
    #[error("convex cache entry count exceeds the supported u32 domain")]
    CountDomainOverflow,
    /// A generated entry exceeded the bounded on-disk size.
    #[error("convex cache entry needs {actual} bytes, exceeding maximum {maximum}")]
    EntryTooLarge {
        /// Encoded entry byte count.
        actual: usize,
        /// Largest accepted entry byte count.
        maximum: u64,
    },
    /// A real filesystem failure occurred; corrupt content itself is not an error.
    #[error("convex cache {operation:?} failed at {path}: {source}")]
    Io {
        /// Operation that failed.
        operation: ConvexCacheIoOperation,
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
}

/// Content-addressed persistent cache for expensive convex decomposition output.
///
/// Each key maps to one checksummed, bounded binary entry. Missing and corrupt
/// entries are cache misses. Real filesystem failures remain errors. Concurrent
/// callers may compute the same miss independently, but publication never
/// exposes a partially written target and no lock is held during generation.
#[derive(Clone, Debug)]
pub struct ConvexDecompositionCache {
    root: PathBuf,
}

impl ConvexDecompositionCache {
    /// Creates a cache rooted at `root` without touching the filesystem.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Loads checked convex parts or generates and atomically publishes a miss.
    ///
    /// The closure is called at most once and only when no valid entry exists.
    /// It should run [`crate::generate_convex_parts`] for the same source and
    /// settings used to derive `key`.
    ///
    /// # Errors
    ///
    /// Returns [`ConvexDecompositionCacheError`] for generation failures,
    /// unsupported output size, or actual filesystem failures. Malformed,
    /// truncated, checksum-invalid, and key-mismatched entries regenerate.
    pub fn get_or_generate<F>(
        &self,
        key: AssetCacheKey,
        generate: F,
    ) -> Result<CachedConvexParts, ConvexDecompositionCacheError>
    where
        F: FnOnce() -> Result<Vec<ConvexPart>, ConvexDecompositionError>,
    {
        let path = self.entry_path(key);
        if let Some(parts) = read_entry(&path, key)? {
            return Ok(CachedConvexParts {
                key,
                disposition: CacheDisposition::Hit,
                parts,
            });
        }

        let parts = generate()?;
        if parts.is_empty() {
            return Err(ConvexDecompositionCacheError::EmptyGeneratedParts);
        }
        let entry = encode_entry(key, &parts)?;
        publish_entry(&path, &entry, key)?;
        Ok(CachedConvexParts {
            key,
            disposition: CacheDisposition::Miss,
            parts,
        })
    }

    fn entry_path(&self, key: AssetCacheKey) -> PathBuf {
        let hex = key.to_hex();
        self.root
            .join("convex-v1")
            .join(&hex[..2])
            .join(format!("{hex}.bin"))
    }
}

fn read_entry(
    path: &Path,
    expected_key: AssetCacheKey,
) -> Result<Option<Vec<ConvexPart>>, ConvexDecompositionCacheError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(ConvexCacheIoOperation::Read, path, source)),
    };
    let length = file
        .metadata()
        .map_err(|source| io_error(ConvexCacheIoOperation::Read, path, source))?
        .len();
    if length > MAX_ENTRY_BYTES {
        return Ok(None);
    }
    let capacity = usize::try_from(length).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(MAX_ENTRY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(ConvexCacheIoOperation::Read, path, source))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ENTRY_BYTES {
        return Ok(None);
    }
    Ok(decode_entry(&bytes, expected_key))
}

fn encode_entry(
    key: AssetCacheKey,
    parts: &[ConvexPart],
) -> Result<Vec<u8>, ConvexDecompositionCacheError> {
    let part_count = u32::try_from(parts.len())
        .map_err(|_| ConvexDecompositionCacheError::CountDomainOverflow)?;
    let counts: Vec<_> = parts
        .iter()
        .map(|part| (part.vertices().len(), part.indices().len()))
        .collect();
    let encoded_length = checked_encoded_length(&counts)?;
    let mut payload = Vec::with_capacity(encoded_length);
    payload.extend_from_slice(ENTRY_MAGIC);
    payload.extend_from_slice(key.as_bytes());
    payload.extend_from_slice(&part_count.to_le_bytes());
    for part in parts {
        let vertex_count = u32::try_from(part.vertices().len())
            .map_err(|_| ConvexDecompositionCacheError::CountDomainOverflow)?;
        let index_count = u32::try_from(part.indices().len())
            .map_err(|_| ConvexDecompositionCacheError::CountDomainOverflow)?;
        payload.extend_from_slice(&vertex_count.to_le_bytes());
        payload.extend_from_slice(&index_count.to_le_bytes());
        for vertex in part.vertices() {
            for component in vertex {
                payload.extend_from_slice(&component.to_bits().to_le_bytes());
            }
        }
        for index in part.indices() {
            payload.extend_from_slice(&index.to_le_bytes());
        }
    }
    let checksum = blake3::hash(&payload);
    payload.extend_from_slice(checksum.as_bytes());
    debug_assert_eq!(payload.len(), encoded_length);
    Ok(payload)
}

fn checked_encoded_length(
    part_counts: &[(usize, usize)],
) -> Result<usize, ConvexDecompositionCacheError> {
    u32::try_from(part_counts.len())
        .map_err(|_| ConvexDecompositionCacheError::CountDomainOverflow)?;
    if part_counts.len() > usize::try_from(MAX_PARTS).expect("u32 fits usize") {
        return Err(ConvexDecompositionCacheError::PartLimitExceeded {
            actual: part_counts.len(),
            maximum: MAX_PARTS,
        });
    }
    let mut total = ENTRY_MAGIC.len() + 32 + 4 + CHECKSUM_BYTES;
    for &(vertex_count, index_count) in part_counts {
        u32::try_from(vertex_count)
            .map_err(|_| ConvexDecompositionCacheError::CountDomainOverflow)?;
        u32::try_from(index_count)
            .map_err(|_| ConvexDecompositionCacheError::CountDomainOverflow)?;
        total = total
            .checked_add(8)
            .and_then(|value| value.checked_add(vertex_count.checked_mul(12)?))
            .and_then(|value| value.checked_add(index_count.checked_mul(4)?))
            .ok_or(ConvexDecompositionCacheError::EntryTooLarge {
                actual: usize::MAX,
                maximum: MAX_ENTRY_BYTES,
            })?;
        if u64::try_from(total).unwrap_or(u64::MAX) > MAX_ENTRY_BYTES {
            return Err(ConvexDecompositionCacheError::EntryTooLarge {
                actual: total,
                maximum: MAX_ENTRY_BYTES,
            });
        }
    }
    Ok(total)
}

fn decode_entry(bytes: &[u8], expected_key: AssetCacheKey) -> Option<Vec<ConvexPart>> {
    if bytes.len() < ENTRY_MAGIC.len() + 32 + 4 + CHECKSUM_BYTES {
        return None;
    }
    let payload_length = bytes.len().checked_sub(CHECKSUM_BYTES)?;
    let (payload, encoded_checksum) = bytes.split_at(payload_length);
    if blake3::hash(payload).as_bytes() != encoded_checksum {
        return None;
    }
    let mut cursor = ByteCursor::new(payload);
    if cursor.take_array::<8>()? != *ENTRY_MAGIC {
        return None;
    }
    if cursor.take_array::<32>()? != *expected_key.as_bytes() {
        return None;
    }
    let part_count = cursor.take_u32()?;
    if part_count == 0 || part_count > MAX_PARTS {
        return None;
    }
    let mut parts = Vec::with_capacity(usize::try_from(part_count).ok()?);
    for _ in 0..part_count {
        let vertex_count = usize::try_from(cursor.take_u32()?).ok()?;
        let index_count = usize::try_from(cursor.take_u32()?).ok()?;
        let vertex_bytes = vertex_count.checked_mul(3)?.checked_mul(4)?;
        let index_bytes = index_count.checked_mul(4)?;
        if vertex_count < 4
            || index_count == 0
            || cursor.remaining() < vertex_bytes.checked_add(index_bytes)?
        {
            return None;
        }
        let mut vertices = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            vertices.push([cursor.take_f32()?, cursor.take_f32()?, cursor.take_f32()?]);
        }
        let mut indices = Vec::with_capacity(index_count);
        for _ in 0..index_count {
            indices.push(cursor.take_u32()?);
        }
        parts.push(ConvexPart::try_new(vertices, indices).ok()?);
    }
    cursor.is_empty().then_some(parts)
}

fn publish_entry(
    target: &Path,
    bytes: &[u8],
    key: AssetCacheKey,
) -> Result<(), ConvexDecompositionCacheError> {
    let parent = target
        .parent()
        .expect("content-addressed cache target always has a parent");
    fs::create_dir_all(parent)
        .map_err(|source| io_error(ConvexCacheIoOperation::CreateDirectory, parent, source))?;
    let (temporary_path, mut temporary) = create_temporary(parent, key)?;
    let temporary_path = TemporaryEntryPath(temporary_path);
    temporary.write_all(bytes).map_err(|source| {
        io_error(
            ConvexCacheIoOperation::WriteTemporary,
            temporary_path.as_ref(),
            source,
        )
    })?;
    drop(temporary);

    let first_error = match fs::rename(temporary_path.as_ref(), target) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    let mut last_error = io::Error::new(first_error.kind(), first_error.to_string());
    for _ in 0..16 {
        if read_entry(target, key)?.is_some() {
            return Ok(());
        }
        match fs::remove_file(target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(io_error(
                    ConvexCacheIoOperation::RemoveCorrupt,
                    target,
                    source,
                ));
            }
        }
        match fs::rename(temporary_path.as_ref(), target) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
    }
    if read_entry(target, key)?.is_some() {
        return Ok(());
    }
    Err(io_error(
        ConvexCacheIoOperation::Publish,
        target,
        io::Error::new(
            last_error.kind(),
            format!("initial publish failed: {first_error}; final retry failed: {last_error}"),
        ),
    ))
}

struct TemporaryEntryPath(PathBuf);

impl AsRef<Path> for TemporaryEntryPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryEntryPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn create_temporary(
    parent: &Path,
    key: AssetCacheKey,
) -> Result<(PathBuf, File), ConvexDecompositionCacheError> {
    for _ in 0..16 {
        let serial = NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{}.tmp-{}-{serial}",
            key.to_hex(),
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(io_error(
                    ConvexCacheIoOperation::CreateTemporary,
                    &path,
                    source,
                ));
            }
        }
    }
    let path = parent.join(format!(".{}.tmp", key.to_hex()));
    Err(io_error(
        ConvexCacheIoOperation::CreateTemporary,
        &path,
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temporary cache filename retries exhausted",
        ),
    ))
}

fn io_error(
    operation: ConvexCacheIoOperation,
    path: &Path,
    source: io::Error,
) -> ConvexDecompositionCacheError {
    ConvexDecompositionCacheError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

struct ByteCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ByteCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take_array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let end = self.offset.checked_add(N)?;
        let bytes = self.bytes.get(self.offset..end)?;
        self.offset = end;
        bytes.try_into().ok()
    }

    fn take_u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take_array()?))
    }

    fn take_f32(&mut self) -> Option<f32> {
        Some(f32::from_bits(self.take_u32()?))
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{ConvexDecompositionCacheError, checked_encoded_length};

    #[test]
    fn oversized_entry_is_rejected_before_buffer_allocation() {
        let error = checked_encoded_length(&[(22_369_620, 3)]).unwrap_err();
        assert!(matches!(
            error,
            ConvexDecompositionCacheError::EntryTooLarge { .. }
        ));
    }
}
