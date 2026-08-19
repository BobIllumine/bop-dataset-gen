use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

use crate::{AppearanceSnapshot, CameraId, CameraSnapshot, SceneSnapshot, SchemaVersion};

/// Versioned set of nonempty output channels requested from a renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RenderChannels {
    schema_version: SchemaVersion,
    rgb: bool,
    depth: bool,
    instance: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderChannelsWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    rgb: bool,
    depth: bool,
    instance: bool,
}

impl<'de> Deserialize<'de> for RenderChannels {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RenderChannelsWire::deserialize(deserializer)?;
        Self::try_new(wire.rgb, wire.depth, wire.instance).map_err(D::Error::custom)
    }
}

impl RenderChannels {
    /// Creates a nonempty requested channel set.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRenderChannelsError`] when every channel is disabled.
    pub const fn try_new(
        rgb: bool,
        depth: bool,
        instance: bool,
    ) -> Result<Self, InvalidRenderChannelsError> {
        if !rgb && !depth && !instance {
            return Err(InvalidRenderChannelsError);
        }
        Ok(Self {
            schema_version: SchemaVersion::current(),
            rgb,
            depth,
            instance,
        })
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(self) -> SchemaVersion {
        self.schema_version
    }

    /// Reports whether RGB output is included.
    #[must_use]
    pub const fn rgb(self) -> bool {
        self.rgb
    }

    /// Reports whether depth output is included.
    #[must_use]
    pub const fn depth(self) -> bool {
        self.depth
    }

    /// Reports whether instance output is included.
    #[must_use]
    pub const fn instance(self) -> bool {
        self.instance
    }

    /// Reports whether this set contains every channel in `required`.
    #[must_use]
    pub const fn contains(self, required: Self) -> bool {
        (!required.rgb || self.rgb)
            && (!required.depth || self.depth)
            && (!required.instance || self.instance)
    }
}

/// Error returned when a render request enables no output channels.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("at least one render channel must be enabled")]
pub struct InvalidRenderChannelsError;

/// Complete versioned renderer input for one immutable scene.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RenderSceneRequest {
    schema_version: SchemaVersion,
    scene: SceneSnapshot,
    cameras: Vec<CameraSnapshot>,
    appearance: AppearanceSnapshot,
    required_channels: RenderChannels,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderSceneRequestWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    scene: SceneSnapshot,
    cameras: Vec<CameraSnapshot>,
    appearance: AppearanceSnapshot,
    required_channels: RenderChannels,
}

impl<'de> Deserialize<'de> for RenderSceneRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RenderSceneRequestWire::deserialize(deserializer)?;
        if let Some(unsorted) = wire
            .cameras
            .windows(2)
            .find(|pair| pair[0].camera_id() > pair[1].camera_id())
        {
            return Err(D::Error::custom(
                InvalidRenderSceneRequestError::UnsortedCameraIds {
                    previous_camera_id: unsorted[0].camera_id(),
                    camera_id: unsorted[1].camera_id(),
                },
            ));
        }
        Self::try_new(
            wire.scene,
            wire.cameras,
            wire.appearance,
            wire.required_channels,
        )
        .map_err(D::Error::custom)
    }
}

impl RenderSceneRequest {
    /// Creates a request and stores cameras in ascending [`CameraId`] order.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRenderSceneRequestError`] when no cameras are supplied
    /// or a camera identifier appears more than once.
    pub fn try_new(
        scene: SceneSnapshot,
        mut cameras: Vec<CameraSnapshot>,
        appearance: AppearanceSnapshot,
        required_channels: RenderChannels,
    ) -> Result<Self, InvalidRenderSceneRequestError> {
        if cameras.is_empty() {
            return Err(InvalidRenderSceneRequestError::EmptyCameras);
        }

        cameras.sort_by_key(|camera| camera.camera_id());
        if let Some(duplicate) = cameras
            .windows(2)
            .find(|pair| pair[0].camera_id() == pair[1].camera_id())
        {
            return Err(InvalidRenderSceneRequestError::DuplicateCameraId {
                camera_id: duplicate[0].camera_id(),
            });
        }

        Ok(Self {
            schema_version: SchemaVersion::current(),
            scene,
            cameras,
            appearance,
            required_channels,
        })
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the immutable physical scene snapshot.
    #[must_use]
    pub const fn scene(&self) -> &SceneSnapshot {
        &self.scene
    }

    /// Returns calibrated cameras in ascending [`CameraId`] order.
    #[must_use]
    pub fn cameras(&self) -> &[CameraSnapshot] {
        &self.cameras
    }

    /// Returns the immutable appearance snapshot.
    #[must_use]
    pub const fn appearance(&self) -> &AppearanceSnapshot {
        &self.appearance
    }

    /// Returns the nonempty set of channels required from the renderer.
    #[must_use]
    pub const fn required_channels(&self) -> RenderChannels {
        self.required_channels
    }
}

/// Error returned when a render-scene request violates camera invariants.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum InvalidRenderSceneRequestError {
    /// No calibrated cameras were supplied.
    #[error("cameras must contain at least one camera")]
    EmptyCameras,
    /// A camera identifier appeared more than once.
    #[error("duplicate camera ID {camera_id:?}")]
    DuplicateCameraId {
        /// The duplicated camera identifier.
        camera_id: CameraId,
    },
    /// Camera IDs on the wire were not sorted in ascending order.
    #[error(
        "wire camera IDs must be sorted ascending, got {camera_id:?} after {previous_camera_id:?}"
    )]
    UnsortedCameraIds {
        /// The preceding larger camera identifier.
        previous_camera_id: CameraId,
        /// The out-of-order camera identifier.
        camera_id: CameraId,
    },
}
