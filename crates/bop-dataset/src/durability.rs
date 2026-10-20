//! Group commit: make many files durable with one device flush.
//!
//! A full device flush (`F_FULLFSYNC` on Apple platforms, the flush inside
//! `fsync` on Linux) costs milliseconds and the device serializes them, so one
//! flush per file caps commit throughput regardless of worker count. A
//! [`DurableBatch`] first hands every file's data to the device without the
//! expensive cache flush, then issues a single flush that covers them all.

use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

/// Files and directories that become durable together at [`DurableBatch::flush`].
#[derive(Default)]
pub(crate) struct DurableBatch {
    last: Option<File>,
}

impl DurableBatch {
    /// Queues one file or directory for the next flush.
    pub(crate) fn add(&mut self, path: &Path) -> Result<(), DurabilityError> {
        let handle = File::open(path).map_err(|source| DurabilityError::new(path, source))?;
        issue(&handle).map_err(|source| DurabilityError::new(path, source))?;
        self.last = Some(handle);
        Ok(())
    }

    /// Queues every file and directory under `root`, including `root` itself.
    pub(crate) fn add_tree(&mut self, root: &Path) -> Result<(), DurabilityError> {
        let mut pending = vec![root.to_owned()];
        while let Some(directory) = pending.pop() {
            let entries = fs::read_dir(&directory)
                .map_err(|source| DurabilityError::new(&directory, source))?;
            for entry in entries {
                let entry = entry.map_err(|source| DurabilityError::new(&directory, source))?;
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .map_err(|source| DurabilityError::new(&path, source))?;
                if file_type.is_dir() {
                    pending.push(path);
                } else {
                    self.add(&path)?;
                }
            }
            self.add(&directory)?;
        }
        Ok(())
    }

    /// Makes everything queued so far durable with one device flush.
    pub(crate) fn flush(self) -> Result<(), DurabilityError> {
        match self.last {
            Some(handle) => barrier(&handle).map_err(|source| DurabilityError {
                path: PathBuf::new(),
                source,
            }),
            None => Ok(()),
        }
    }
}

/// Makes one file or directory durable immediately with its own flush.
pub(crate) fn sync_now(path: &Path) -> Result<(), DurabilityError> {
    let mut batch = DurableBatch::default();
    batch.add(path)?;
    batch.flush().map_err(|error| DurabilityError {
        path: path.to_owned(),
        source: error.source,
    })
}

/// Hands a file's data and metadata to the device without forcing its cache.
#[cfg(target_vendor = "apple")]
fn issue(handle: &File) -> io::Result<()> {
    rustix::fs::fsync(handle).map_err(io::Error::from)
}

/// Defers all work to the single `syncfs` barrier.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "matches the per-platform signature"
)]
const fn issue(_handle: &File) -> io::Result<()> {
    Ok(())
}

/// Falls back to a full durable sync per file where no cheaper split exists.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn issue(handle: &File) -> io::Result<()> {
    handle.sync_all()
}

/// Flushes the device cache, covering every write issued before it.
#[cfg(target_vendor = "apple")]
fn barrier(handle: &File) -> io::Result<()> {
    rustix::fs::fcntl_fullfsync(handle).map_err(io::Error::from)
}

/// Writes back and flushes every dirty page of the filesystem.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn barrier(handle: &File) -> io::Result<()> {
    rustix::fs::syncfs(handle).map_err(io::Error::from)
}

/// Every file was already synced individually.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn barrier(_handle: &File) -> io::Result<()> {
    Ok(())
}

/// A file or directory could not be made durable.
#[derive(Debug)]
pub(crate) struct DurabilityError {
    pub(crate) path: PathBuf,
    pub(crate) source: io::Error,
}

impl DurabilityError {
    fn new(path: &Path, source: io::Error) -> Self {
        Self {
            path: path.to_owned(),
            source,
        }
    }
}
