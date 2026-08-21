use std::{collections::BTreeMap, num::NonZeroU32};

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

use crate::{NonNegativeFinite, RenderChannels, SceneId, SchemaVersion};

/// Renderer implementation family used for capability and provenance records.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    /// In-process renderer implemented with `wgpu`.
    Wgpu,
    /// Out-of-process `BlenderProc` renderer.
    BlenderProc,
}

/// Versioned renderer identity and deterministic implementation metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BackendInfo {
    schema_version: SchemaVersion,
    kind: BackendKind,
    implementation_version: String,
    device_name: Option<String>,
    properties: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendInfoWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    kind: BackendKind,
    implementation_version: String,
    device_name: Option<String>,
    properties: BTreeMap<String, String>,
}

impl<'de> Deserialize<'de> for BackendInfo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BackendInfoWire::deserialize(deserializer)?;
        Self::try_new(
            wire.kind,
            wire.implementation_version,
            wire.device_name,
            wire.properties,
        )
        .map_err(D::Error::custom)
    }
}

impl BackendInfo {
    /// Creates renderer provenance with checked textual metadata.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidBackendInfoError`] when the implementation version,
    /// optional device name, or a property key or value is empty or
    /// whitespace-only.
    pub fn try_new(
        kind: BackendKind,
        implementation_version: impl Into<String>,
        device_name: Option<String>,
        properties: BTreeMap<String, String>,
    ) -> Result<Self, InvalidBackendInfoError> {
        let implementation_version = implementation_version.into();
        if implementation_version.trim().is_empty() {
            return Err(InvalidBackendInfoError::InvalidImplementationVersion {
                value: implementation_version,
            });
        }
        if let Some(value) = device_name.as_ref().filter(|value| value.trim().is_empty()) {
            return Err(InvalidBackendInfoError::InvalidDeviceName {
                value: value.clone(),
            });
        }
        for (key, value) in &properties {
            if key.trim().is_empty() {
                return Err(InvalidBackendInfoError::InvalidPropertyKey { key: key.clone() });
            }
            if value.trim().is_empty() {
                return Err(InvalidBackendInfoError::InvalidPropertyValue {
                    key: key.clone(),
                    value: value.clone(),
                });
            }
        }

        Ok(Self {
            schema_version: SchemaVersion::current(),
            kind,
            implementation_version,
            device_name,
            properties,
        })
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the renderer implementation family.
    #[must_use]
    pub const fn kind(&self) -> BackendKind {
        self.kind
    }

    /// Returns the renderer implementation version.
    #[must_use]
    pub fn implementation_version(&self) -> &str {
        &self.implementation_version
    }

    /// Returns the device name when the backend reports one.
    #[must_use]
    pub fn device_name(&self) -> Option<&str> {
        self.device_name.as_deref()
    }

    /// Returns deterministically ordered backend properties.
    #[must_use]
    pub const fn properties(&self) -> &BTreeMap<String, String> {
        &self.properties
    }
}

/// Error returned when renderer provenance contains blank text.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum InvalidBackendInfoError {
    /// The implementation version is empty or whitespace-only.
    #[error("implementation_version must contain a non-whitespace character, got {value:?}")]
    InvalidImplementationVersion {
        /// The rejected implementation version.
        value: String,
    },
    /// The present device name is empty or whitespace-only.
    #[error("device_name must contain a non-whitespace character when present, got {value:?}")]
    InvalidDeviceName {
        /// The rejected device name.
        value: String,
    },
    /// A property key is empty or whitespace-only.
    #[error("backend property key must contain a non-whitespace character, got {key:?}")]
    InvalidPropertyKey {
        /// The rejected property key.
        key: String,
    },
    /// A property value is empty or whitespace-only.
    #[error("backend property {key:?} must have a non-whitespace value, got {value:?}")]
    InvalidPropertyValue {
        /// The property whose value was rejected.
        key: String,
        /// The rejected property value.
        value: String,
    },
}

/// Versioned renderer channel support and optional per-scene camera limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RendererCapabilities {
    schema_version: SchemaVersion,
    channels: RenderChannels,
    max_cameras_per_scene: Option<NonZeroU32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RendererCapabilitiesWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    channels: RenderChannels,
    max_cameras_per_scene: Option<NonZeroU32>,
}

impl<'de> Deserialize<'de> for RendererCapabilities {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RendererCapabilitiesWire::deserialize(deserializer)?;
        Ok(Self::new(wire.channels, wire.max_cameras_per_scene))
    }
}

impl RendererCapabilities {
    /// Creates renderer capabilities; `None` declares no camera limit.
    #[must_use]
    pub const fn new(channels: RenderChannels, max_cameras_per_scene: Option<NonZeroU32>) -> Self {
        Self {
            schema_version: SchemaVersion::current(),
            channels,
            max_cameras_per_scene,
        }
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns all channels supported by the renderer.
    #[must_use]
    pub const fn channels(self) -> RenderChannels {
        self.channels
    }

    /// Returns the optional maximum camera count per scene.
    #[must_use]
    pub const fn max_cameras_per_scene(self) -> Option<NonZeroU32> {
        self.max_cameras_per_scene
    }

    /// Checks requested channel containment and a nonzero camera count.
    #[must_use]
    pub const fn supports(self, channels: RenderChannels, camera_count: u32) -> bool {
        if camera_count == 0 || !self.channels.contains(channels) {
            return false;
        }
        match self.max_cameras_per_scene {
            Some(limit) => camera_count <= limit.get(),
            None => true,
        }
    }
}

/// Versioned durations for the stages of one render operation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RenderStageTimings {
    schema_version: SchemaVersion,
    prepare_seconds: NonNegativeFinite,
    render_seconds: NonNegativeFinite,
    readback_seconds: NonNegativeFinite,
    encode_seconds: NonNegativeFinite,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderStageTimingsWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    prepare_seconds: NonNegativeFinite,
    render_seconds: NonNegativeFinite,
    readback_seconds: NonNegativeFinite,
    encode_seconds: NonNegativeFinite,
}

impl<'de> Deserialize<'de> for RenderStageTimings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RenderStageTimingsWire::deserialize(deserializer)?;
        Ok(Self::new(
            wire.prepare_seconds,
            wire.render_seconds,
            wire.readback_seconds,
            wire.encode_seconds,
        ))
    }
}

impl RenderStageTimings {
    /// Creates per-stage render timings from checked durations.
    #[must_use]
    pub const fn new(
        prepare_seconds: NonNegativeFinite,
        render_seconds: NonNegativeFinite,
        readback_seconds: NonNegativeFinite,
        encode_seconds: NonNegativeFinite,
    ) -> Self {
        Self {
            schema_version: SchemaVersion::current(),
            prepare_seconds,
            render_seconds,
            readback_seconds,
            encode_seconds,
        }
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the preparation duration.
    #[must_use]
    pub const fn prepare_seconds(self) -> NonNegativeFinite {
        self.prepare_seconds
    }

    /// Returns the rendering duration.
    #[must_use]
    pub const fn render_seconds(self) -> NonNegativeFinite {
        self.render_seconds
    }

    /// Returns the readback duration.
    #[must_use]
    pub const fn readback_seconds(self) -> NonNegativeFinite {
        self.readback_seconds
    }

    /// Returns the encoding duration.
    #[must_use]
    pub const fn encode_seconds(self) -> NonNegativeFinite {
        self.encode_seconds
    }

    /// Sums all stage durations, returning `None` on finite-`f32` overflow.
    #[must_use]
    pub fn total_seconds(self) -> Option<NonNegativeFinite> {
        NonNegativeFinite::try_new(
            self.prepare_seconds.get()
                + self.render_seconds.get()
                + self.readback_seconds.get()
                + self.encode_seconds.get(),
        )
        .ok()
    }
}

/// Versioned measurements and renderer provenance for one rendered scene.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RenderReport {
    schema_version: SchemaVersion,
    scene_id: SceneId,
    backend: BackendInfo,
    camera_count: NonZeroU32,
    channels: RenderChannels,
    timings: RenderStageTimings,
    worker_restart_count: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderReportWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    scene_id: SceneId,
    backend: BackendInfo,
    camera_count: NonZeroU32,
    channels: RenderChannels,
    timings: RenderStageTimings,
    worker_restart_count: u32,
}

impl<'de> Deserialize<'de> for RenderReport {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RenderReportWire::deserialize(deserializer)?;
        Ok(Self::new(
            wire.scene_id,
            wire.backend,
            wire.camera_count,
            wire.channels,
            wire.timings,
            wire.worker_restart_count,
        ))
    }
}

impl RenderReport {
    /// Creates a render report from checked renderer-facing values.
    #[must_use]
    pub const fn new(
        scene_id: SceneId,
        backend: BackendInfo,
        camera_count: NonZeroU32,
        channels: RenderChannels,
        timings: RenderStageTimings,
        worker_restart_count: u32,
    ) -> Self {
        Self {
            schema_version: SchemaVersion::current(),
            scene_id,
            backend,
            camera_count,
            channels,
            timings,
            worker_restart_count,
        }
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the rendered scene identifier.
    #[must_use]
    pub const fn scene_id(&self) -> SceneId {
        self.scene_id
    }

    /// Returns renderer provenance for the rendered scene.
    #[must_use]
    pub const fn backend(&self) -> &BackendInfo {
        &self.backend
    }

    /// Returns the nonzero number of rendered cameras.
    #[must_use]
    pub const fn camera_count(&self) -> NonZeroU32 {
        self.camera_count
    }

    /// Returns the channels included in the render output.
    #[must_use]
    pub const fn channels(&self) -> RenderChannels {
        self.channels
    }

    /// Returns the per-stage render timings.
    #[must_use]
    pub const fn timings(&self) -> RenderStageTimings {
        self.timings
    }

    /// Returns the recorded worker restart count.
    #[must_use]
    pub const fn worker_restart_count(&self) -> u32 {
        self.worker_restart_count
    }
}
