//! Persistent, versioned `BlenderProc` worker supervision.

use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use bpp_assets::{ResolvedAssetCatalog, Texture};
use bpp_core::{
    BackendInfo, BackendKind, BlenderProcRendererConfig, NonNegativeFinite, RenderError,
    RenderReport, RenderSceneRequest, RenderStageTimings, Renderer, RendererCapabilities,
};
use bpp_dataset::{DepthFrame, InstanceFrame, RgbFrame, SceneStagingArea};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

/// Exact JSON-lines protocol version shared with the Python worker.
pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Serde guard that accepts only [`PROTOCOL_VERSION`].
#[doc(hidden)]
pub struct ProtocolVersion;

impl Serialize for ProtocolVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u16(PROTOCOL_VERSION)
    }
}

impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let version = u16::deserialize(deserializer)?;
        if version == PROTOCOL_VERSION {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported BlenderProc protocol version {version}; expected {PROTOCOL_VERSION}"
            )))
        }
    }
}

/// One controller-to-worker JSON-lines message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerMessage {
    /// Starts a worker session and selects its rendering device.
    Hello {
        /// Exact protocol compatibility guard.
        protocol_version: ProtocolVersion,
        /// `BlenderProc` device-selection expression.
        device_selection: String,
    },
}

impl WorkerMessage {
    /// Constructs the mandatory first protocol message.
    #[must_use]
    pub fn hello(device_selection: impl Into<String>) -> Self {
        Self::Hello {
            protocol_version: ProtocolVersion,
            device_selection: device_selection.into(),
        }
    }
}

/// One worker-to-controller JSON-lines reply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerReply {
    /// Confirms protocol compatibility and reports worker provenance.
    Ready {
        /// Exact protocol compatibility guard.
        protocol_version: ProtocolVersion,
        /// `BlenderProc` or worker implementation version.
        implementation_version: String,
        /// Selected device, when the runtime reports one.
        device_name: Option<String>,
    },
    /// Confirms that all catalog assets were loaded.
    Prepared {
        /// Exact protocol compatibility guard.
        protocol_version: ProtocolVersion,
        /// Prepared catalog fingerprint.
        catalog_fingerprint: String,
    },
    /// Confirms raw output for one scene is complete.
    Rendered {
        /// Exact protocol compatibility guard.
        protocol_version: ProtocolVersion,
        /// Rendered scene identifier.
        scene_id: u64,
        /// Worker-reported render duration.
        render_seconds: f32,
    },
    /// Confirms all worker-side operations are durable.
    Flushed {
        /// Exact protocol compatibility guard.
        protocol_version: ProtocolVersion,
    },
    /// Reports a request-level worker failure without crashing the process.
    Error {
        /// Exact protocol compatibility guard.
        protocol_version: ProtocolVersion,
        /// Stable worker error category.
        code: String,
        /// Human-readable diagnostic.
        message: String,
    },
}

impl WorkerReply {
    /// Returns the implementation version when this is a readiness reply.
    #[must_use]
    pub fn implementation_version(&self) -> Option<&str> {
        match self {
            Self::Ready {
                implementation_version,
                ..
            } => Some(implementation_version),
            Self::Prepared { .. }
            | Self::Rendered { .. }
            | Self::Flushed { .. }
            | Self::Error { .. } => None,
        }
    }
}

/// Persistent `BlenderProc` subprocess implementing the renderer boundary.
pub struct BlenderProcRenderer {
    config: BlenderProcRendererConfig,
    info: BackendInfo,
    capabilities: RendererCapabilities,
    worker: Worker,
    prepared_fingerprint: Option<String>,
}

impl BlenderProcRenderer {
    /// Starts `BlenderProc` and completes the bounded protocol handshake.
    ///
    /// # Errors
    ///
    /// Returns a typed process, timeout, or protocol initialization error.
    pub fn try_new(
        config: BlenderProcRendererConfig,
    ) -> Result<Self, BlenderProcInitializationError> {
        let mut worker = Worker::spawn(&config)?;
        worker.send(&WorkerMessage::hello(config.device_selection()))?;
        let ready = worker.receive(Duration::from_secs_f32(config.handshake_timeout_s()))?;
        let (implementation_version, device_name) = match ready {
            WorkerReply::Ready {
                implementation_version,
                device_name,
                ..
            } => (implementation_version, device_name),
            reply => {
                return Err(BlenderProcInitializationError::UnexpectedReply(reply_kind(
                    &reply,
                )));
            }
        };
        let info = BackendInfo::try_new(
            BackendKind::BlenderProc,
            implementation_version,
            device_name,
            BTreeMap::from([("protocol_version".to_owned(), PROTOCOL_VERSION.to_string())]),
        )?;
        let channels = bpp_core::RenderChannels::try_new(true, true, true)
            .map_err(|_| BlenderProcInitializationError::InternalInvariant)?;
        Ok(Self {
            config,
            info,
            capabilities: RendererCapabilities::new(channels, None),
            worker,
            prepared_fingerprint: None,
        })
    }

    /// Returns immutable worker provenance.
    #[must_use]
    pub fn info(&self) -> BackendInfo {
        self.info.clone()
    }
}

impl Renderer for BlenderProcRenderer {
    type AssetCatalog = ResolvedAssetCatalog;
    type StagingArea = SceneStagingArea;

    fn info(&self) -> BackendInfo {
        self.info()
    }

    fn capabilities(&self) -> RendererCapabilities {
        self.capabilities
    }

    fn prepare(&mut self, assets: &Self::AssetCatalog) -> Result<(), RenderError> {
        let payload = json!({
            "protocol_version": PROTOCOL_VERSION,
            "type": "prepare",
            "catalog_fingerprint": assets.fingerprint(),
            "assets": catalog_wire(assets),
        });
        self.worker
            .send(&payload)
            .map_err(|error| initialization_as_render(&error))?;
        match self.receive_request()? {
            WorkerReply::Prepared {
                catalog_fingerprint,
                ..
            } if catalog_fingerprint == assets.fingerprint() => {
                self.prepared_fingerprint = Some(catalog_fingerprint);
                Ok(())
            }
            WorkerReply::Error { code, message, .. } => Err(worker_error(&code, &message)),
            reply => Err(protocol_error(format!(
                "expected prepared, received {}",
                reply_kind(&reply)
            ))),
        }
    }

    fn render_scene(
        &mut self,
        request: &RenderSceneRequest,
        staging: &Self::StagingArea,
    ) -> Result<RenderReport, RenderError> {
        if self.prepared_fingerprint.is_none() {
            return Err(RenderError::NotPrepared);
        }
        let camera_count = u32::try_from(request.cameras().len()).map_err(|error| {
            RenderError::InvalidContent {
                message: error.to_string(),
            }
        })?;
        self.capabilities
            .require(request.required_channels(), camera_count)?;
        self.worker
            .send(&json!({
                "protocol_version": PROTOCOL_VERSION,
                "type": "render",
                "request": request,
                "output_root": staging.root(),
            }))
            .map_err(|error| initialization_as_render(&error))?;
        let render_seconds = match self.receive_request()? {
            WorkerReply::Rendered {
                scene_id,
                render_seconds,
                ..
            } if scene_id == request.scene().scene_id().get() => {
                NonNegativeFinite::try_new(render_seconds)
                    .map_err(|error| protocol_error(error.to_string()))?
            }
            WorkerReply::Error { code, message, .. } => return Err(worker_error(&code, &message)),
            reply => {
                return Err(protocol_error(format!(
                    "expected rendered, received {}",
                    reply_kind(&reply)
                )));
            }
        };
        let encode_started = Instant::now();
        encode_raw_outputs(request, staging)?;
        let encode_seconds = duration_value(encode_started.elapsed())?;
        let zero = NonNegativeFinite::try_new(0.0).map_err(|error| RenderError::Internal {
            message: error.to_string(),
        })?;
        let camera_count =
            std::num::NonZeroU32::new(camera_count).ok_or_else(|| RenderError::Internal {
                message: "render request unexpectedly had no cameras".to_owned(),
            })?;
        Ok(RenderReport::new(
            request.scene().scene_id(),
            self.info.clone(),
            camera_count,
            request.required_channels(),
            RenderStageTimings::new(zero, render_seconds, zero, encode_seconds),
            0,
        ))
    }

    fn flush(&mut self) -> Result<(), RenderError> {
        self.worker
            .send(&json!({"protocol_version": PROTOCOL_VERSION, "type": "flush"}))
            .map_err(|error| initialization_as_render(&error))?;
        match self.receive_request()? {
            WorkerReply::Flushed { .. } => Ok(()),
            WorkerReply::Error { code, message, .. } => Err(worker_error(&code, &message)),
            reply => Err(protocol_error(format!(
                "expected flushed, received {}",
                reply_kind(&reply)
            ))),
        }
    }
}

#[derive(Serialize)]
struct AssetWire<'a> {
    asset_id: &'a str,
    primitives: Vec<PrimitiveWire<'a>>,
    materials: Vec<MaterialWire>,
    textures: Vec<TextureWire<'a>>,
}

#[derive(Serialize)]
struct PrimitiveWire<'a> {
    positions: &'a [[f32; 3]],
    normals: &'a [[f32; 3]],
    tex_coord_sets: &'a [Vec<[f32; 2]>],
    indices: &'a [u32],
    material: Option<u32>,
}

#[derive(Serialize)]
struct MaterialWire {
    base_color_factor: [f32; 4],
    metallic_factor: f32,
    roughness_factor: f32,
    base_color_texture: Option<TextureBindingWire>,
}

#[derive(Serialize)]
struct TextureBindingWire {
    texture: u32,
    tex_coord_set: u32,
}

#[derive(Serialize)]
struct TextureWire<'a> {
    dimensions: [u32; 2],
    rgba8_srgb: &'a [u8],
}

fn catalog_wire(catalog: &ResolvedAssetCatalog) -> Vec<AssetWire<'_>> {
    catalog
        .iter()
        .map(|entry| {
            let asset = entry.asset();
            AssetWire {
                asset_id: entry.asset_id().as_str(),
                primitives: asset
                    .visual_mesh()
                    .primitives()
                    .iter()
                    .map(|primitive| PrimitiveWire {
                        positions: primitive.positions(),
                        normals: primitive.normals(),
                        tex_coord_sets: primitive.tex_coord_sets(),
                        indices: primitive.indices(),
                        material: primitive.material().map(bpp_assets::MaterialIndex::get),
                    })
                    .collect(),
                materials: asset
                    .materials()
                    .iter()
                    .map(|material| MaterialWire {
                        base_color_factor: material.base_color_factor(),
                        metallic_factor: material.metallic_factor(),
                        roughness_factor: material.roughness_factor(),
                        base_color_texture: material.base_color_texture().map(|binding| {
                            TextureBindingWire {
                                texture: binding.texture().get(),
                                tex_coord_set: binding.tex_coord_set(),
                            }
                        }),
                    })
                    .collect(),
                textures: asset.textures().iter().map(texture_wire).collect(),
            }
        })
        .collect()
}

fn texture_wire(texture: &Texture) -> TextureWire<'_> {
    TextureWire {
        dimensions: texture.image().dimensions(),
        rgba8_srgb: texture.image().rgba8_srgb(),
    }
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: mpsc::Receiver<Result<String, std::io::Error>>,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
}

impl Worker {
    fn spawn(config: &BlenderProcRendererConfig) -> Result<Self, BlenderProcInitializationError> {
        let mut child = Command::new(config.executable_path())
            .arg("run")
            .arg(config.worker_path())
            .arg("--")
            .arg("--bpp-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(BlenderProcInitializationError::Spawn)?;
        let stdin = child
            .stdin
            .take()
            .ok_or(BlenderProcInitializationError::MissingPipe("stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or(BlenderProcInitializationError::MissingPipe("stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(BlenderProcInitializationError::MissingPipe("stderr"))?;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_capture = Arc::clone(&stderr_tail);
        let capacity = usize::try_from(config.stderr_tail_capacity()).unwrap_or(usize::MAX);
        thread::spawn(move || {
            for byte in BufReader::new(stderr).bytes().flatten() {
                if let Ok(mut tail) = stderr_capture.lock() {
                    if tail.len() == capacity {
                        tail.pop_front();
                    }
                    tail.push_back(byte);
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            stdout: receiver,
            stderr_tail,
        })
    }

    fn send<T: Serialize>(&mut self, message: &T) -> Result<(), BlenderProcInitializationError> {
        serde_json::to_writer(&mut self.stdin, message)
            .map_err(BlenderProcInitializationError::Serialize)?;
        self.stdin
            .write_all(b"\n")
            .map_err(BlenderProcInitializationError::Write)?;
        self.stdin
            .flush()
            .map_err(BlenderProcInitializationError::Write)
    }

    fn receive(
        &mut self,
        timeout: Duration,
    ) -> Result<WorkerReply, BlenderProcInitializationError> {
        match self.stdout.recv_timeout(timeout) {
            Ok(Ok(line)) => {
                serde_json::from_str(&line).map_err(BlenderProcInitializationError::Protocol)
            }
            Ok(Err(error)) => Err(BlenderProcInitializationError::Read(error)),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(BlenderProcInitializationError::Timeout {
                stderr_tail: self.stderr_tail(),
            }),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(BlenderProcInitializationError::Exited {
                    status: self
                        .child
                        .try_wait()
                        .ok()
                        .flatten()
                        .map(|status| status.to_string()),
                    stderr_tail: self.stderr_tail(),
                })
            }
        }
    }

    fn stderr_tail(&self) -> String {
        self.stderr_tail.lock().map_or_else(
            |_| "<stderr capture poisoned>".to_owned(),
            |mut tail| String::from_utf8_lossy(tail.make_contiguous()).into_owned(),
        )
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.send(&json!({"protocol_version": PROTOCOL_VERSION, "type": "shutdown"}));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl BlenderProcRenderer {
    fn receive_request(&mut self) -> Result<WorkerReply, RenderError> {
        self.worker
            .receive(Duration::from_secs_f32(self.config.request_timeout_s()))
            .map_err(|error| initialization_as_render(&error))
    }
}

/// Failure while starting or handshaking with the `BlenderProc` worker.
#[derive(Debug, Error)]
pub enum BlenderProcInitializationError {
    /// Executable could not be started.
    #[error("failed to spawn BlenderProc: {0}")]
    Spawn(#[source] std::io::Error),
    /// A required subprocess pipe was unavailable.
    #[error("BlenderProc subprocess did not expose {0}")]
    MissingPipe(&'static str),
    /// A protocol message could not be serialized.
    #[error("failed to serialize BlenderProc protocol message: {0}")]
    Serialize(#[source] serde_json::Error),
    /// A protocol message could not be written.
    #[error("failed to write BlenderProc protocol message: {0}")]
    Write(#[source] std::io::Error),
    /// Worker stdout failed.
    #[error("failed to read BlenderProc worker stdout: {0}")]
    Read(#[source] std::io::Error),
    /// Worker emitted malformed or incompatible JSON.
    #[error("invalid BlenderProc worker reply: {0}")]
    Protocol(#[source] serde_json::Error),
    /// Worker did not answer before the configured deadline.
    #[error("BlenderProc worker timed out; stderr tail: {stderr_tail}")]
    Timeout {
        /// Bounded diagnostic captured from worker stderr.
        stderr_tail: String,
    },
    /// Worker exited before replying.
    #[error("BlenderProc worker exited ({status:?}); stderr tail: {stderr_tail}")]
    Exited {
        /// Exit status, if it was observable.
        status: Option<String>,
        /// Bounded diagnostic captured from worker stderr.
        stderr_tail: String,
    },
    /// Worker replied with a different message kind.
    #[error("expected BlenderProc ready reply, received {0}")]
    UnexpectedReply(&'static str),
    /// Worker provenance violated the shared backend contract.
    #[error(transparent)]
    InvalidProvenance(#[from] bpp_core::InvalidBackendInfoError),
    /// A compile-time renderer capability invariant failed.
    #[error("internal BlenderProc capability invariant failed")]
    InternalInvariant,
}

fn initialization_as_render(error: &BlenderProcInitializationError) -> RenderError {
    match error {
        BlenderProcInitializationError::Timeout { .. } => RenderError::Timeout {
            message: error.to_string(),
        },
        BlenderProcInitializationError::Exited { .. } => RenderError::WorkerCrashed {
            message: error.to_string(),
        },
        BlenderProcInitializationError::Protocol(_)
        | BlenderProcInitializationError::UnexpectedReply(_) => RenderError::Protocol {
            message: error.to_string(),
        },
        _ => RenderError::BackendUnavailable {
            message: error.to_string(),
        },
    }
}

fn worker_error(code: &str, message: &str) -> RenderError {
    RenderError::InvalidContent {
        message: format!("BlenderProc worker {code}: {message}"),
    }
}

fn protocol_error(message: String) -> RenderError {
    RenderError::Protocol { message }
}

fn duration_value(duration: Duration) -> Result<NonNegativeFinite, RenderError> {
    NonNegativeFinite::try_new(duration.as_secs_f32()).map_err(|error| RenderError::Internal {
        message: error.to_string(),
    })
}

fn reply_kind(reply: &WorkerReply) -> &'static str {
    match reply {
        WorkerReply::Ready { .. } => "ready",
        WorkerReply::Prepared { .. } => "prepared",
        WorkerReply::Rendered { .. } => "rendered",
        WorkerReply::Flushed { .. } => "flushed",
        WorkerReply::Error { .. } => "error",
    }
}

fn encode_raw_outputs(
    request: &RenderSceneRequest,
    staging: &SceneStagingArea,
) -> Result<(), RenderError> {
    for camera in request.cameras() {
        let size = camera.image_size();
        let directory = staging.camera_dir(camera.camera_id());
        if request.required_channels().rgb() {
            let path = directory.join("rgb.u8");
            let bytes = read_exact_raw(&path, size.width(), size.height(), 3)?;
            RgbFrame::try_new(size.width(), size.height(), bytes)
                .map_err(|error| protocol_error(error.to_string()))?
                .write_png(&staging.rgb_path(camera.camera_id()))
                .map_err(|error| RenderError::Io {
                    operation: "encode BlenderProc RGB".to_owned(),
                    message: error.to_string(),
                })?;
            remove_raw(&path)?;
        }
        if request.required_channels().depth() {
            let path = directory.join("depth.f32le");
            let bytes = read_exact_raw(&path, size.width(), size.height(), 4)?;
            let values = bytes
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("exact chunk")))
                .collect();
            DepthFrame::try_new(size.width(), size.height(), values)
                .map_err(|error| protocol_error(error.to_string()))?
                .write_exr(&staging.depth_path(camera.camera_id()))
                .map_err(|error| RenderError::Io {
                    operation: "encode BlenderProc depth".to_owned(),
                    message: error.to_string(),
                })?;
            remove_raw(&path)?;
        }
        if request.required_channels().instance() {
            let path = directory.join("instance.u16le");
            let bytes = read_exact_raw(&path, size.width(), size.height(), 2)?;
            let values = bytes
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes(chunk.try_into().expect("exact chunk")))
                .collect();
            InstanceFrame::try_new(size.width(), size.height(), values)
                .map_err(|error| protocol_error(error.to_string()))?
                .write_png(&staging.instance_path(camera.camera_id()))
                .map_err(|error| RenderError::Io {
                    operation: "encode BlenderProc instance".to_owned(),
                    message: error.to_string(),
                })?;
            remove_raw(&path)?;
        }
    }
    Ok(())
}

fn read_exact_raw(
    path: &std::path::Path,
    width: u32,
    height: u32,
    bytes_per_pixel: u64,
) -> Result<Vec<u8>, RenderError> {
    let expected = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
        .ok_or_else(|| protocol_error("raw frame size overflow".to_owned()))?;
    let bytes = fs::read(path).map_err(|error| RenderError::Io {
        operation: format!("read raw frame {}", path.display()),
        message: error.to_string(),
    })?;
    if u64::try_from(bytes.len()).ok() != Some(expected) {
        return Err(protocol_error(format!(
            "raw frame {} has {} bytes; expected {expected}",
            path.display(),
            bytes.len()
        )));
    }
    Ok(bytes)
}

fn remove_raw(path: &std::path::Path) -> Result<(), RenderError> {
    fs::remove_file(path).map_err(|error| RenderError::Io {
        operation: format!("remove raw frame {}", path.display()),
        message: error.to_string(),
    })
}
