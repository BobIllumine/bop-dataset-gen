use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use bpp_core::{CameraId, SceneId};

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

impl Drop for SceneStagingArea {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
