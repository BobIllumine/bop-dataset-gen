//! Durable object storage: an S3-compatible bucket or a local directory.
//!
//! Keys are `/`-separated: `assets/<path>` for uploaded source files and
//! `datasets/<job-id>/...` for results, and `exports/<job-id>/<format>.tar`
//! for packed downloads.

use std::{path::Path as LocalPath, sync::Arc, time::Duration};

use futures::TryStreamExt as _;
use object_store::{
    GetResult, ObjectStore, ObjectStoreExt as _, PutPayload, WriteMultipart,
    aws::{AmazonS3, AmazonS3Builder},
    local::LocalFileSystem,
    path::Path,
    prefix::PrefixStore,
    signer::{Method, Signer as _},
};
use thiserror::Error;

/// A storage operation failed.
#[derive(Debug, Error)]
pub enum StorageError {
    /// The storage URL scheme is not `s3://` or `file://`.
    #[error("storage URL {0:?} must start with s3:// or file://")]
    UnsupportedUrl(String),
    /// The object store rejected the operation.
    #[error(transparent)]
    Store(#[from] object_store::Error),
    /// A local file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// Local path.
        path: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// One stored object.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct StoredObject {
    /// Key relative to the listed prefix.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
}

/// Cheaply cloneable handle to the configured store.
#[derive(Clone, Debug)]
pub struct Storage {
    store: Arc<dyn ObjectStore>,
    /// The bucket and key prefix, for presigned URLs; `None` for a local directory.
    signer: Option<(Arc<AmazonS3>, String)>,
}

/// Tar block size; every header and every entry's data is padded to it.
const TAR_BLOCK: usize = 512;
/// Multipart parts allowed in flight while packing, bounding memory use.
const PACK_CONCURRENCY: usize = 8;

impl Storage {
    /// Opens `s3://bucket[/prefix]` or `file:///absolute/directory`.
    ///
    /// S3 credentials, region, and endpoint come from the standard `AWS_*`
    /// variables; for `MinIO` set `AWS_ENDPOINT_URL` and, over plain HTTP,
    /// `AWS_ALLOW_HTTP=true`. A local directory is created if missing.
    ///
    /// # Errors
    ///
    /// Returns why the URL or store configuration is invalid.
    pub fn from_url(url: &str) -> Result<Self, StorageError> {
        if let Some(rest) = url.strip_prefix("s3://") {
            let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
            let s3 = Arc::new(
                AmazonS3Builder::from_env()
                    .with_bucket_name(bucket)
                    .build()?,
            );
            let prefix = prefix.trim_matches('/').to_owned();
            let store: Arc<dyn ObjectStore> = if prefix.is_empty() {
                s3.clone()
            } else {
                Arc::new(PrefixStore::new(Arc::clone(&s3), prefix.as_str()))
            };
            Ok(Self {
                store,
                signer: Some((s3, prefix)),
            })
        } else if let Some(directory) = url.strip_prefix("file://") {
            std::fs::create_dir_all(directory).map_err(|source| StorageError::Io {
                path: directory.to_owned(),
                source,
            })?;
            Ok(Self {
                store: Arc::new(LocalFileSystem::new_with_prefix(directory)?),
                signer: None,
            })
        } else {
            Err(StorageError::UnsupportedUrl(url.to_owned()))
        }
    }

    /// Reports whether [`Self::signed_get_url`] can return presigned URLs.
    #[must_use]
    pub const fn can_sign(&self) -> bool {
        self.signer.is_some()
    }

    /// Returns a URL that downloads `key` without credentials until `expires_in`
    /// passes, or `None` when the store cannot sign (a local directory).
    ///
    /// # Errors
    ///
    /// Returns the signing failure.
    pub async fn signed_get_url(
        &self,
        key: &str,
        expires_in: Duration,
    ) -> Result<Option<String>, StorageError> {
        let Some((s3, prefix)) = &self.signer else {
            return Ok(None);
        };
        let full = if prefix.is_empty() {
            Path::from(key)
        } else {
            Path::from(format!("{prefix}/{key}"))
        };
        let url = s3.signed_url(Method::GET, &full, expires_in).await?;
        Ok(Some(url.to_string()))
    }

    /// Returns the size of the object at `key`, or `None` if it is missing.
    ///
    /// # Errors
    ///
    /// Returns store failures other than a missing object.
    pub async fn size(&self, key: &str) -> Result<Option<u64>, StorageError> {
        match self.store.head(&Path::from(key)).await {
            Ok(meta) => Ok(Some(meta.size)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Packs every object below `prefix` whose relative key `keep` accepts into
    /// an uncompressed tar stored at `key`, streaming without a local copy.
    /// Entries are named `<root>/<key relative to prefix>`, in key order.
    ///
    /// # Errors
    ///
    /// Returns the store failure.
    pub async fn pack_tar(
        &self,
        prefix: &str,
        keep: impl Fn(&str) -> bool,
        root: &str,
        key: &str,
    ) -> Result<u64, StorageError> {
        let objects = self.list(prefix).await?;
        let mut upload = WriteMultipart::new(self.store.put_multipart(&Path::from(key)).await?);
        let mut written = 0_u64;
        for object in objects.iter().filter(|object| keep(&object.path)) {
            upload.wait_for_capacity(PACK_CONCURRENCY).await?;
            let header = tar_header(&format!("{root}/{}", object.path), object.size);
            written += header.len() as u64;
            upload.write(&header);
            let mut body = self
                .store
                .get(&Path::from(format!("{prefix}/{}", object.path)))
                .await?
                .into_stream();
            while let Some(chunk) = body.try_next().await? {
                upload.wait_for_capacity(PACK_CONCURRENCY).await?;
                written += chunk.len() as u64;
                upload.put(chunk);
            }
            let padding = padding(object.size);
            written += padding as u64;
            upload.write(&[0; TAR_BLOCK][..padding]);
        }
        // Two empty blocks end the archive.
        upload.write(&[0; 2 * TAR_BLOCK]);
        written += 2 * TAR_BLOCK as u64;
        upload.finish().await?;
        Ok(written)
    }

    /// Stores `bytes` at `key`, replacing any existing object.
    ///
    /// # Errors
    ///
    /// Returns the store failure.
    pub async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), StorageError> {
        self.store
            .put(&Path::from(key), PutPayload::from(bytes))
            .await?;
        Ok(())
    }

    /// Stores the local file at `key`.
    ///
    /// # Errors
    ///
    /// Returns the read or store failure.
    pub async fn put_file(&self, key: &str, local: &LocalPath) -> Result<(), StorageError> {
        let bytes = tokio::fs::read(local)
            .await
            .map_err(|source| StorageError::Io {
                path: local.display().to_string(),
                source,
            })?;
        self.put(key, bytes).await
    }

    /// Writes the object at `key` to `local`, creating parent directories.
    ///
    /// # Errors
    ///
    /// Returns the store or write failure; a missing object is
    /// `StorageError::Store(object_store::Error::NotFound { .. })`.
    pub async fn download(&self, key: &str, local: &LocalPath) -> Result<(), StorageError> {
        let bytes = self.store.get(&Path::from(key)).await?.bytes().await?;
        let io = |source| StorageError::Io {
            path: local.display().to_string(),
            source,
        };
        if let Some(parent) = local.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(io)?;
        }
        tokio::fs::write(local, bytes).await.map_err(io)
    }

    /// Opens the object at `key` for streaming.
    ///
    /// # Errors
    ///
    /// Returns the store failure.
    pub async fn get(&self, key: &str) -> Result<GetResult, StorageError> {
        Ok(self.store.get(&Path::from(key)).await?)
    }

    /// Reports whether an object exists at `key`.
    ///
    /// # Errors
    ///
    /// Returns store failures other than a missing object.
    pub async fn exists(&self, key: &str) -> Result<bool, StorageError> {
        match self.store.head(&Path::from(key)).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Lists every object below `prefix`, sorted by key.
    ///
    /// # Errors
    ///
    /// Returns the store failure.
    pub async fn list(&self, prefix: &str) -> Result<Vec<StoredObject>, StorageError> {
        let prefix = Path::from(prefix);
        let mut objects = self
            .store
            .list(Some(&prefix))
            .map_ok(|meta| StoredObject {
                path: meta.location.prefix_match(&prefix).map_or_else(
                    || meta.location.to_string(),
                    |parts| {
                        parts
                            .map(|part| part.as_ref().to_owned())
                            .collect::<Vec<_>>()
                            .join("/")
                    },
                ),
                size: meta.size,
            })
            .try_collect::<Vec<_>>()
            .await?;
        objects.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(objects)
    }

    /// Deletes the object at `key`.
    ///
    /// # Errors
    ///
    /// Returns the store failure.
    pub async fn delete(&self, key: &str) -> Result<(), StorageError> {
        Ok(self.store.delete(&Path::from(key)).await?)
    }
}

/// Zero bytes that pad `size` bytes of entry data to a whole block.
// The result is below one block, so it always fits.
#[allow(clippy::cast_possible_truncation)]
const fn padding(size: u64) -> usize {
    let block = TAR_BLOCK as u64;
    ((block - size % block) % block) as usize
}

/// The header block(s) for a regular file `name` of `size` bytes. Names too
/// long for the ustar header are preceded by a GNU long-name entry.
fn tar_header(name: &str, size: u64) -> Vec<u8> {
    let mut blocks = Vec::with_capacity(TAR_BLOCK);
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_size(size);
    header.set_entry_type(tar::EntryType::Regular);
    if header.set_path(name).is_err() {
        let mut long = tar::Header::new_gnu();
        long.set_mode(0o644);
        long.set_mtime(0);
        long.set_entry_type(tar::EntryType::GNULongName);
        // The name is NUL-terminated inside its data blocks.
        let data_len = name.len() as u64 + 1;
        long.set_size(data_len);
        long.as_old_mut().name[..13].copy_from_slice(b"././@LongLink");
        long.set_cksum();
        blocks.extend_from_slice(long.as_bytes());
        blocks.extend_from_slice(name.as_bytes());
        blocks.push(0);
        blocks.resize(blocks.len() + padding(data_len), 0);
        let truncated = &name.as_bytes()[..100];
        header.as_old_mut().name.copy_from_slice(truncated);
    }
    header.set_cksum();
    blocks.extend_from_slice(header.as_bytes());
    blocks
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::{padding, tar_header};

    #[test]
    fn headers_round_trip_through_a_tar_reader_including_long_names() {
        let long = format!("scenes/{}/rgb.png", "x".repeat(150));
        let mut archive = Vec::new();
        for (name, data) in [("manifest.json", b"{}".as_slice()), (long.as_str(), b"png")] {
            archive.extend(tar_header(name, data.len() as u64));
            archive.extend_from_slice(data);
            archive.resize(archive.len() + padding(data.len() as u64), 0);
        }
        archive.resize(archive.len() + 1024, 0);
        let mut reader = tar::Archive::new(archive.as_slice());
        let entries = reader
            .entries()
            .unwrap()
            .map(|entry| {
                let mut entry = entry.unwrap();
                let path = entry.path().unwrap().display().to_string();
                let mut body = String::new();
                entry.read_to_string(&mut body).unwrap();
                (path, body)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            [
                ("manifest.json".to_owned(), "{}".to_owned()),
                (long, "png".to_owned())
            ]
        );
    }
}
