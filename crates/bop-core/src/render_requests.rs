use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

use crate::{
    AppearanceSnapshot, BinRenderMaterial, CameraId, CameraSnapshot, InstanceId, SceneSnapshot,
    SchemaVersion, SrgbColor,
};

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

/// How hard the PNG encoder compresses RGB and instance frames. The pixels
/// are identical at every level; only file size and encoding time differ.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PngCompression {
    /// Specialized fast DEFLATE: about 13x faster than `Balanced` on
    /// rendered frames, files about 30% larger.
    #[default]
    Fast,
    /// zlib-style default compression.
    Balanced,
    /// Slowest; a few percent smaller than `Balanced`.
    High,
}

/// Complete versioned renderer input for one immutable scene.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RenderSceneRequest {
    schema_version: SchemaVersion,
    scene: SceneSnapshot,
    cameras: Vec<CameraSnapshot>,
    appearance: AppearanceSnapshot,
    required_channels: RenderChannels,
    png_compression: PngCompression,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    instance_materials: Vec<InstanceMaterial>,
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
    #[serde(default)]
    png_compression: PngCompression,
    #[serde(default)]
    instance_materials: Vec<InstanceMaterial>,
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
        .and_then(|request| {
            request
                .with_png_compression(wire.png_compression)
                .with_instance_materials(wire.instance_materials)
        })
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
            png_compression: PngCompression::default(),
            instance_materials: Vec::new(),
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

    /// Returns how RGB and instance PNGs are compressed.
    #[must_use]
    pub const fn png_compression(&self) -> PngCompression {
        self.png_compression
    }

    /// Replaces the PNG compression level.
    #[must_use]
    pub const fn with_png_compression(mut self, png_compression: PngCompression) -> Self {
        self.png_compression = png_compression;
        self
    }

    /// Returns the per-instance materials in ascending instance order.
    #[must_use]
    pub fn instance_materials(&self) -> &[InstanceMaterial] {
        &self.instance_materials
    }

    /// Returns the material chosen for `instance_id`, if any.
    #[must_use]
    pub fn instance_material(&self, instance_id: InstanceId) -> Option<&InstanceMaterial> {
        self.instance_materials
            .binary_search_by_key(&instance_id, |material| material.instance_id)
            .ok()
            .map(|index| &self.instance_materials[index])
    }

    /// Replaces the per-instance materials, sorted by instance.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRenderSceneRequestError`] when an instance has two
    /// materials or is not in the scene.
    pub fn with_instance_materials(
        mut self,
        mut materials: Vec<InstanceMaterial>,
    ) -> Result<Self, InvalidRenderSceneRequestError> {
        materials.sort_by_key(|material| material.instance_id);
        if let Some(pair) = materials
            .windows(2)
            .find(|pair| pair[0].instance_id == pair[1].instance_id)
        {
            return Err(InvalidRenderSceneRequestError::DuplicateInstanceMaterial {
                instance_id: pair[0].instance_id,
            });
        }
        if let Some(stray) = materials.iter().find(|material| {
            !self
                .scene
                .instances()
                .iter()
                .any(|instance| instance.instance_id() == material.instance_id)
        }) {
            return Err(InvalidRenderSceneRequestError::UnknownInstanceMaterial {
                instance_id: stray.instance_id,
            });
        }
        self.instance_materials = materials;
        Ok(self)
    }
}

/// Which primitives of an instance an [`InstanceMaterial`] applies to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialScope {
    /// Every primitive: replaces the asset file's color factor, metallic and
    /// roughness (base-color textures stay and are tinted by the new color).
    All,
    /// Only primitives whose file names no material, such as every STL.
    Unassigned,
}

/// The surface one scene instance is rendered with.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct InstanceMaterial {
    instance_id: InstanceId,
    scope: MaterialScope,
    base_color_srgb: SrgbColor,
    metallic: f32,
    roughness: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstanceMaterialWire {
    instance_id: InstanceId,
    scope: MaterialScope,
    base_color_srgb: SrgbColor,
    metallic: f32,
    roughness: f32,
}

impl<'de> Deserialize<'de> for InstanceMaterial {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = InstanceMaterialWire::deserialize(deserializer)?;
        let surface =
            BinRenderMaterial::try_new(wire.base_color_srgb, wire.metallic, wire.roughness)
                .map_err(D::Error::custom)?;
        Ok(Self::new(wire.instance_id, wire.scope, surface))
    }
}

impl InstanceMaterial {
    /// Assigns the checked `surface` to `instance_id`.
    #[must_use]
    pub const fn new(
        instance_id: InstanceId,
        scope: MaterialScope,
        surface: BinRenderMaterial,
    ) -> Self {
        Self {
            instance_id,
            scope,
            base_color_srgb: surface.base_color_srgb(),
            metallic: surface.metallic(),
            roughness: surface.roughness(),
        }
    }

    /// Returns the instance this material belongs to.
    #[must_use]
    pub const fn instance_id(&self) -> InstanceId {
        self.instance_id
    }

    /// Returns which primitives it applies to.
    #[must_use]
    pub const fn scope(&self) -> MaterialScope {
        self.scope
    }

    /// Returns the sRGB base color factor.
    #[must_use]
    pub const fn base_color_srgb(&self) -> SrgbColor {
        self.base_color_srgb
    }

    /// Returns the metallic factor in `[0, 1]`.
    #[must_use]
    pub const fn metallic(&self) -> f32 {
        self.metallic
    }

    /// Returns the roughness factor in `[0, 1]`.
    #[must_use]
    pub const fn roughness(&self) -> f32 {
        self.roughness
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
    /// An instance was given more than one material.
    #[error("instance {instance_id:?} has more than one material")]
    DuplicateInstanceMaterial {
        /// The instance with several materials.
        instance_id: InstanceId,
    },
    /// A material names an instance the scene does not contain.
    #[error("material for instance {instance_id:?}, which is not in the scene")]
    UnknownInstanceMaterial {
        /// The missing instance.
        instance_id: InstanceId,
    },
}
