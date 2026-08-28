use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use bpp_core::RenderChannels;
use bpp_core::{CameraId, SceneId};

/// Expected per-camera artifacts for one rendered scene.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneArtifactRequirements {
    camera_ids: Vec<CameraId>,
    channels: RenderChannels,
}

impl SceneArtifactRequirements {
    /// Creates requirements in canonical camera-identifier order.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an empty camera set or duplicate identifier.
    pub fn try_new(
        mut camera_ids: Vec<CameraId>,
        channels: RenderChannels,
    ) -> Result<Self, InvalidSceneArtifactRequirements> {
        if camera_ids.is_empty() {
            return Err(InvalidSceneArtifactRequirements::EmptyCameras);
        }
        camera_ids.sort_unstable();
        if let Some(duplicate) = camera_ids.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(InvalidSceneArtifactRequirements::DuplicateCamera {
                camera_id: duplicate[0],
            });
        }
        Ok(Self {
            camera_ids,
            channels,
        })
    }
}

/// Why scene artifact requirements are invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidSceneArtifactRequirements {
    /// No camera was supplied.
    EmptyCameras,
    /// A camera identifier occurs more than once.
    DuplicateCamera {
        /// Repeated identifier.
        camera_id: CameraId,
    },
}

impl std::fmt::Display for InvalidSceneArtifactRequirements {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyCameras => {
                formatter.write_str("scene artifacts require at least one camera")
            }
            Self::DuplicateCamera { camera_id } => {
                write!(
                    formatter,
                    "camera {} occurs more than once",
                    camera_id.get()
                )
            }
        }
    }
}

impl std::error::Error for InvalidSceneArtifactRequirements {}

/// Failure while validating or atomically publishing a staged scene.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SceneCommitError {
    /// A required artifact is absent or is not a regular file.
    MissingArtifact {
        /// Canonical path relative to the scene root.
        relative_path: PathBuf,
    },
    /// A required artifact is empty.
    EmptyArtifact {
        /// Canonical path relative to the scene root.
        relative_path: PathBuf,
    },
    /// The canonical destination is already occupied.
    DestinationExists {
        /// Scene whose existing output was protected.
        scene_id: SceneId,
    },
    /// A filesystem operation failed.
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Portable I/O error category.
        kind: io::ErrorKind,
    },
}

impl std::fmt::Display for SceneCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingArtifact { relative_path } => {
                write!(
                    formatter,
                    "required artifact {} is missing",
                    relative_path.display()
                )
            }
            Self::EmptyArtifact { relative_path } => {
                write!(
                    formatter,
                    "required artifact {} is empty",
                    relative_path.display()
                )
            }
            Self::DestinationExists { scene_id } => {
                write!(formatter, "scene {} is already committed", scene_id.get())
            }
            Self::Io { path, kind } => {
                write!(
                    formatter,
                    "filesystem operation on {} failed: {kind:?}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for SceneCommitError {}

static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(0);

/// Unique filesystem area populated before one scene is validated and committed.
///
/// Paths follow the canonical dataset layout beneath a private `.staging`
/// directory. Dropping an uncommitted area removes it recursively.
pub struct SceneStagingArea {
    scene_id: SceneId,
    root: PathBuf,
    cleanup_on_drop: bool,
}

impl SceneStagingArea {
    /// Creates a unique empty staging directory below `dataset_root/.staging`.
    ///
    /// # Errors
    ///
    /// Returns the underlying filesystem error if the staging parent or a
    /// unique child directory cannot be created.
    pub fn create(dataset_root: &Path, scene_id: SceneId) -> io::Result<Self> {
        let staging_parent = dataset_root.join(".staging");
        fs::create_dir_all(&staging_parent)?;
        for _ in 0..1024 {
            let nonce = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
            let candidate = staging_parent.join(format!(
                "scene-{:020}-{}-{nonce:020}",
                scene_id.get(),
                std::process::id()
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    return Ok(Self {
                        scene_id,
                        root: candidate,
                        cleanup_on_drop: true,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique scene staging directory after 1024 attempts",
        ))
    }

    /// Returns the scene identity assigned to this staging area.
    #[must_use]
    pub const fn scene_id(&self) -> SceneId {
        self.scene_id
    }

    /// Returns the unique staging root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the canonical scene metadata path.
    #[must_use]
    pub fn scene_metadata_path(&self) -> PathBuf {
        self.root().join("scene.json")
    }

    /// Returns one camera's canonical frame directory without creating it.
    #[must_use]
    pub fn camera_dir(&self, camera_id: CameraId) -> PathBuf {
        self.root()
            .join("frames")
            .join(format!("{:05}", camera_id.get()))
    }

    /// Creates one camera frame directory, accepting an existing directory.
    ///
    /// # Errors
    ///
    /// Returns the underlying filesystem error, including when a non-directory
    /// entry blocks the canonical path.
    pub fn prepare_camera(&self, camera_id: CameraId) -> io::Result<PathBuf> {
        let path = self.camera_dir(camera_id);
        fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// Returns one camera's canonical frame metadata path.
    #[must_use]
    pub fn frame_metadata_path(&self, camera_id: CameraId) -> PathBuf {
        self.camera_dir(camera_id).join("frame.json")
    }

    /// Returns one camera's canonical sRGB PNG path.
    #[must_use]
    pub fn rgb_path(&self, camera_id: CameraId) -> PathBuf {
        self.camera_dir(camera_id).join("rgb.png")
    }

    /// Returns one camera's canonical metric-depth EXR path.
    #[must_use]
    pub fn depth_path(&self, camera_id: CameraId) -> PathBuf {
        self.camera_dir(camera_id).join("depth.exr")
    }

    /// Returns one camera's canonical visible-instance PNG path.
    #[must_use]
    pub fn instance_path(&self, camera_id: CameraId) -> PathBuf {
        self.camera_dir(camera_id).join("instance.png")
    }

    /// Validates required artifacts, synchronizes them, and renames the whole
    /// staging tree into the canonical `scenes/<scene-id>` destination.
    ///
    /// Existing scene output is never intentionally replaced. Any error leaves
    /// no newly published scene, and dropping this value cleans its staging tree.
    ///
    /// # Errors
    ///
    /// Returns a typed validation, collision, or filesystem error.
    pub fn commit(
        mut self,
        requirements: &SceneArtifactRequirements,
    ) -> Result<PathBuf, SceneCommitError> {
        let relative_paths = required_artifacts(requirements);
        for relative_path in &relative_paths {
            let path = self.root.join(relative_path);
            let metadata = match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => metadata,
                Ok(_) => {
                    return Err(SceneCommitError::MissingArtifact {
                        relative_path: relative_path.clone(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Err(SceneCommitError::MissingArtifact {
                        relative_path: relative_path.clone(),
                    });
                }
                Err(error) => return Err(io_error(path, &error)),
            };
            if metadata.len() == 0 {
                return Err(SceneCommitError::EmptyArtifact {
                    relative_path: relative_path.clone(),
                });
            }
            File::open(&path)
                .and_then(|file| file.sync_all())
                .map_err(|error| io_error(path, &error))?;
        }
        sync_directory(&self.root)?;

        let dataset_root =
            self.root
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| SceneCommitError::Io {
                    path: self.root.clone(),
                    kind: io::ErrorKind::InvalidInput,
                })?;
        let scenes = dataset_root.join("scenes");
        fs::create_dir_all(&scenes).map_err(|error| io_error(scenes.clone(), &error))?;
        sync_directory(&scenes)?;
        let destination = scenes.join(format!("{:020}", self.scene_id.get()));
        if destination.exists() {
            return Err(SceneCommitError::DestinationExists {
                scene_id: self.scene_id,
            });
        }
        fs::rename(&self.root, &destination)
            .map_err(|error| io_error(destination.clone(), &error))?;
        sync_directory(&scenes)?;
        self.cleanup_on_drop = false;
        Ok(destination)
    }

    /// Explicitly removes this staging area and reports cleanup failures.
    ///
    /// # Errors
    ///
    /// Returns the underlying recursive-removal error.
    pub fn discard(mut self) -> io::Result<()> {
        self.cleanup_on_drop = false;
        fs::remove_dir_all(&self.root)
    }
}

fn required_artifacts(requirements: &SceneArtifactRequirements) -> Vec<PathBuf> {
    let mut paths = Vec::with_capacity(1 + requirements.camera_ids.len() * 4);
    paths.push(PathBuf::from("scene.json"));
    for camera_id in &requirements.camera_ids {
        let directory = PathBuf::from("frames").join(format!("{:05}", camera_id.get()));
        paths.push(directory.join("frame.json"));
        if requirements.channels.rgb() {
            paths.push(directory.join("rgb.png"));
        }
        if requirements.channels.depth() {
            paths.push(directory.join("depth.exr"));
        }
        if requirements.channels.instance() {
            paths.push(directory.join("instance.png"));
        }
    }
    paths
}

fn sync_directory(path: &Path) -> Result<(), SceneCommitError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error(path.to_owned(), &error))
}

fn io_error(path: PathBuf, error: &io::Error) -> SceneCommitError {
    SceneCommitError::Io {
        path,
        kind: error.kind(),
    }
}

impl Drop for SceneStagingArea {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
