//! Durable object storage: an S3-compatible bucket or a local directory.
//!
//! Keys are `/`-separated: `assets/<path>` for uploaded source files and
//! `datasets/<job-id>/...` for results, and `exports/<job-id>/<format>.tar`
//! for packed downloads.

use std::{path::Path as LocalPath, sync::Arc, time::Duration};

use futures::{StreamExt as _, TryStreamExt as _};
use object_store::{
    GetResult, ObjectStore, ObjectStoreExt as _, PutPayload, WriteMultipart,
    aws::{AmazonS3, AmazonS3Builder},
    local::LocalFileSystem,
    path::Path,
    prefix::PrefixStore,
    signer::{Method, Signer as _},
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};

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

/// One stored object with its entity tag.
#[derive(Clone, Debug)]
pub struct TaggedObject {
    /// Key relative to the listed prefix.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Entity tag; changes whenever the object is replaced.
    pub etag: Option<String>,
}

/// Cheaply cloneable handle to the configured store.
#[derive(Clone, Debug)]
pub struct Storage {
    store: Arc<dyn ObjectStore>,
    /// The bucket and key prefix, for presigned URLs; `None` for a local directory.
    signer: Option<(Arc<AmazonS3>, String)>,
    /// Requests a bulk operation keeps in flight.
    concurrency: usize,
}

/// Tar block size; every header and every entry's data is padded to it.
const TAR_BLOCK: usize = 512;
/// Multipart parts allowed in flight while packing, bounding memory use.
const PACK_CONCURRENCY: usize = 8;
/// Requests a bulk operation keeps in flight unless [`Storage::with_concurrency`] says otherwise.
const DEFAULT_CONCURRENCY: usize = 32;
/// Files larger than this are uploaded in parts, streamed from disk.
const MULTIPART_THRESHOLD: u64 = 8 * 1024 * 1024;
/// Bytes read from a local file at a time while streaming it.
const READ_CHUNK: u64 = 1024 * 1024;

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
                concurrency: DEFAULT_CONCURRENCY,
            })
        } else if let Some(directory) = url.strip_prefix("file://") {
            std::fs::create_dir_all(directory).map_err(|source| StorageError::Io {
                path: directory.to_owned(),
                source,
            })?;
            Ok(Self {
                store: Arc::new(LocalFileSystem::new_with_prefix(directory)?),
                signer: None,
                concurrency: DEFAULT_CONCURRENCY,
            })
        } else {
            Err(StorageError::UnsupportedUrl(url.to_owned()))
        }
    }

    /// Keeps up to `concurrency` (at least one) requests in flight in bulk operations.
    #[must_use]
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// Requests a bulk operation keeps in flight.
    #[must_use]
    pub const fn concurrency(&self) -> usize {
        self.concurrency
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
    /// Objects are fetched ahead, [`Self::concurrency`] at a time.
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
        let objects = self
            .list(prefix)
            .await?
            .into_iter()
            .filter(|object| keep(&object.path))
            .collect::<Vec<_>>();
        let mut tar = TarUpload::start(self.store.as_ref(), key).await?;
        let filled = async {
            let mut bodies = futures::stream::iter(objects)
                .map(|object| async move {
                    let location = Path::from(format!("{prefix}/{}", object.path));
                    let bytes = self.store.get(&location).await?.bytes().await?;
                    Ok::<_, object_store::Error>((object.path, bytes))
                })
                .buffered(self.concurrency);
            while let Some((path, bytes)) = bodies.try_next().await? {
                let size = bytes.len() as u64;
                tar.begin(&format!("{root}/{path}"), size).await?;
                tar.data(bytes).await?;
                tar.end(size);
            }
            Ok(())
        }
        .await;
        tar.finish(filled).await
    }

    /// Packs local files, given as `(path relative to local_root, size)`, into
    /// an uncompressed tar stored at `key`. Entries are named `<root>/<path>`
    /// in the order given, so sorted paths give the same bytes as
    /// [`Self::pack_tar`] over the same files.
    ///
    /// # Errors
    ///
    /// Returns the read or store failure, including a file whose size changed.
    pub async fn pack_local_tar(
        &self,
        local_root: &LocalPath,
        files: &[(String, u64)],
        root: &str,
        key: &str,
    ) -> Result<u64, StorageError> {
        let mut tar = TarUpload::start(self.store.as_ref(), key).await?;
        // Files up to the multipart threshold are read ahead, several at a
        // time; larger ones are streamed when their turn comes. Futures built
        // up front: a mapping closure would make callers' futures not `Send`.
        let reads = files
            .iter()
            .map(|(path, size)| {
                let local = local_root.join(path);
                async move {
                    if *size > MULTIPART_THRESHOLD {
                        return Ok(None);
                    }
                    tokio::fs::read(&local)
                        .await
                        .map(Some)
                        .map_err(|source| io_error(&local, source))
                }
            })
            .collect::<Vec<_>>();
        let mut prefetched = futures::stream::iter(reads)
            .buffered(self.concurrency)
            .zip(futures::stream::iter(files));
        let filled = async {
            while let Some((ahead, (path, size))) = prefetched.next().await {
                let local = local_root.join(path);
                let ahead = ahead?;
                tar.begin(&format!("{root}/{path}"), *size).await?;
                let mut read = 0;
                if let Some(bytes) = ahead {
                    read = bytes.len() as u64;
                    if read == *size {
                        tar.data(bytes.into()).await?;
                    }
                } else {
                    let mut file = open(&local).await?.take(*size);
                    loop {
                        let chunk = read_chunk(&mut file, &local).await?;
                        if chunk.is_empty() {
                            break;
                        }
                        read += chunk.len() as u64;
                        tar.data(chunk.into()).await?;
                    }
                }
                if read != *size {
                    return Err(io_error(
                        &local,
                        std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "file changed size while packing",
                        ),
                    ));
                }
                tar.end(*size);
            }
            Ok(())
        }
        .await;
        tar.finish(filled).await
    }

    /// Stores `bytes` at `key`, replacing any existing object.
    ///
    /// Returns the stored object's entity tag, when the store reports one.
    ///
    /// # Errors
    ///
    /// Returns the store failure.
    pub async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<Option<String>, StorageError> {
        Ok(self
            .store
            .put(&Path::from(key), PutPayload::from(bytes))
            .await?
            .e_tag)
    }

    /// Stores the local file at `key`; files above 8 MiB are streamed in parts.
    ///
    /// # Errors
    ///
    /// Returns the read or store failure.
    pub async fn put_file(&self, key: &str, local: &LocalPath) -> Result<(), StorageError> {
        let size = tokio::fs::metadata(local)
            .await
            .map_err(|source| io_error(local, source))?
            .len();
        if size <= MULTIPART_THRESHOLD {
            let bytes = tokio::fs::read(local)
                .await
                .map_err(|source| io_error(local, source))?;
            self.put(key, bytes).await?;
            return Ok(());
        }
        let mut file = open(local).await?;
        let mut upload = WriteMultipart::new(self.store.put_multipart(&Path::from(key)).await?);
        let streamed = async {
            loop {
                let chunk = read_chunk(&mut file, local).await?;
                if chunk.is_empty() {
                    return Ok(());
                }
                upload.wait_for_capacity(PACK_CONCURRENCY).await?;
                upload.put(chunk.into());
            }
        }
        .await;
        match streamed {
            Ok(()) => {
                upload.finish().await?;
                Ok(())
            }
            Err(error) => {
                let _ = upload.abort().await;
                Err(error)
            }
        }
    }

    /// Streams the object at `key` to `local`, creating parent directories.
    /// The file appears complete or not at all.
    ///
    /// # Errors
    ///
    /// Returns the store or write failure; a missing object is
    /// `StorageError::Store(object_store::Error::NotFound { .. })`.
    pub async fn download(&self, key: &str, local: &LocalPath) -> Result<(), StorageError> {
        let mut body = self.store.get(&Path::from(key)).await?.into_stream();
        if let Some(parent) = local.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| io_error(local, source))?;
        }
        let name = local
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let partial = local.with_file_name(format!(".{name}.part"));
        let copied = async {
            let mut file = tokio::fs::File::create(&partial)
                .await
                .map_err(|source| io_error(&partial, source))?;
            while let Some(chunk) = body.try_next().await? {
                file.write_all(&chunk)
                    .await
                    .map_err(|source| io_error(&partial, source))?;
            }
            file.flush()
                .await
                .map_err(|source| io_error(&partial, source))?;
            tokio::fs::rename(&partial, local)
                .await
                .map_err(|source| io_error(local, source))
        }
        .await;
        if copied.is_err() {
            let _ = tokio::fs::remove_file(&partial).await;
        }
        copied
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
        Ok(self
            .list_tagged(prefix)
            .await?
            .into_iter()
            .map(|object| StoredObject {
                path: object.path,
                size: object.size,
            })
            .collect())
    }

    /// Lists every object below `prefix` with its entity tag, sorted by key.
    ///
    /// # Errors
    ///
    /// Returns the store failure.
    pub async fn list_tagged(&self, prefix: &str) -> Result<Vec<TaggedObject>, StorageError> {
        let prefix = Path::from(prefix);
        let mut objects = self
            .store
            .list(Some(&prefix))
            .map_ok(|meta| TaggedObject {
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
                etag: meta.e_tag,
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

/// A tar archive being written to a multipart upload.
struct TarUpload {
    upload: WriteMultipart,
    written: u64,
}

impl TarUpload {
    async fn start(store: &dyn ObjectStore, key: &str) -> Result<Self, StorageError> {
        Ok(Self {
            upload: WriteMultipart::new(store.put_multipart(&Path::from(key)).await?),
            written: 0,
        })
    }

    /// Writes the header of a regular file `name` of `size` bytes.
    async fn begin(&mut self, name: &str, size: u64) -> Result<(), StorageError> {
        self.upload.wait_for_capacity(PACK_CONCURRENCY).await?;
        let header = tar_header(name, size);
        self.written += header.len() as u64;
        self.upload.write(&header);
        Ok(())
    }

    /// Appends entry data.
    async fn data(&mut self, chunk: axum::body::Bytes) -> Result<(), StorageError> {
        self.upload.wait_for_capacity(PACK_CONCURRENCY).await?;
        self.written += chunk.len() as u64;
        self.upload.put(chunk);
        Ok(())
    }

    /// Pads an entry of `size` bytes to a whole block.
    fn end(&mut self, size: u64) {
        let padding = padding(size);
        self.written += padding as u64;
        self.upload.write(&[0; TAR_BLOCK][..padding]);
    }

    /// Ends the archive and completes the upload when `filled` succeeded,
    /// else abandons the upload. Returns the archive size.
    async fn finish(mut self, filled: Result<(), StorageError>) -> Result<u64, StorageError> {
        if let Err(error) = filled {
            let _ = self.upload.abort().await;
            return Err(error);
        }
        // Two empty blocks end the archive.
        self.upload.write(&[0; 2 * TAR_BLOCK]);
        self.written += 2 * TAR_BLOCK as u64;
        self.upload.finish().await?;
        Ok(self.written)
    }
}

fn io_error(path: &LocalPath, source: std::io::Error) -> StorageError {
    StorageError::Io {
        path: path.display().to_string(),
        source,
    }
}

async fn open(path: &LocalPath) -> Result<tokio::fs::File, StorageError> {
    tokio::fs::File::open(path)
        .await
        .map_err(|source| io_error(path, source))
}

/// Reads up to [`READ_CHUNK`] bytes; empty at the end of `reader`.
async fn read_chunk(
    reader: &mut (impl AsyncRead + Unpin),
    path: &LocalPath,
) -> Result<Vec<u8>, StorageError> {
    let mut chunk = Vec::new();
    reader
        .take(READ_CHUNK)
        .read_to_end(&mut chunk)
        .await
        .map_err(|source| io_error(path, source))?;
    Ok(chunk)
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

    use super::{Storage, padding, tar_header};

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

    #[tokio::test]
    async fn packing_local_files_matches_packing_the_same_stored_objects() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::from_url(&format!("file://{}/store", root.path().display()))
            .unwrap()
            .with_concurrency(3);
        let local = root.path().join("dataset");
        let long = format!("scenes/{}/rgb.png", "x".repeat(150));
        let files = [
            ("run.json", vec![b'{'; 3]),
            ("scenes/0/frames/1/depth.exr", vec![7; 513]),
            ("scenes/0/scene.json", vec![]),
            (long.as_str(), vec![1; 1024]),
            // Above the read-ahead limit, so streamed in chunks.
            (
                "scenes/0/large.bin",
                (0..=250_u8).cycle().take(9 * 1024 * 1024).collect(),
            ),
        ];
        let mut listed = Vec::new();
        for (path, bytes) in &files {
            let file = local.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, bytes).unwrap();
            storage
                .put(&format!("datasets/job/{path}"), bytes.clone())
                .await
                .unwrap();
            listed.push(((*path).to_owned(), bytes.len() as u64));
        }
        listed.sort();

        let from_store = storage
            .pack_tar("datasets/job", |_| true, "dataset-job", "exports/a.tar")
            .await
            .unwrap();
        let from_disk = storage
            .pack_local_tar(&local, &listed, "dataset-job", "exports/b.tar")
            .await
            .unwrap();
        let read =
            |name: &str| std::fs::read(root.path().join("store/exports").join(name)).unwrap();
        assert_eq!(from_store, from_disk);
        assert_eq!(read("a.tar"), read("b.tar"));
        assert_eq!(read("a.tar").len() as u64, from_store);
    }
}
