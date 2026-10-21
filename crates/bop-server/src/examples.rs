//! Example meshes for a new asset library.
//!
//! The server ships a few primitive meshes, the ones the studio's presets use,
//! and copies them into an asset library that has never received them. A
//! marker in storage records that it did, so examples a user deletes stay
//! deleted; a file already in the library is never replaced.

use std::path::Path;

use crate::storage::{Storage, StorageError};

/// Marks a store whose library has received the examples. It lies outside
/// `assets/`, so it is not listed as an asset.
pub const SEEDED_MARKER: &str = ".bop/examples-seeded";

/// Copies every file in `directory` to `assets/<name>`, skipping names the
/// library already has, unless the store has received the examples before.
/// Returns the names copied.
///
/// # Errors
///
/// Returns the read or store failure. The marker is written last, so a
/// failed attempt is completed at the next start.
pub async fn seed(storage: &Storage, directory: &Path) -> Result<Vec<String>, StorageError> {
    if storage.exists(SEEDED_MARKER).await? {
        return Ok(Vec::new());
    }
    let io = |source| StorageError::Io {
        path: directory.display().to_string(),
        source,
    };
    let mut names = Vec::new();
    let mut entries = tokio::fs::read_dir(directory).await.map_err(io)?;
    while let Some(entry) = entries.next_entry().await.map_err(io)? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') && entry.file_type().await.map_err(io)?.is_file() {
            names.push(name);
        }
    }
    names.sort();
    let mut copied = Vec::new();
    for name in names {
        let key = format!("assets/{name}");
        if !storage.exists(&key).await? {
            storage.put_file(&key, &directory.join(&name)).await?;
            copied.push(name);
        }
    }
    storage.put(SEEDED_MARKER, Vec::new()).await?;
    Ok(copied)
}
