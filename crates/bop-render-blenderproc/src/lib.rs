//! Persistent, versioned `BlenderProc` worker supervision.
//!
//! [`BlenderProcRenderer`] starts one external worker process
//! (`tools/blenderproc-worker/blenderproc_worker.py`), speaks a line-based
//! JSON protocol with it ([`WorkerMessage`], [`WorkerReply`]), restarts it once
//! if it dies mid-scene, and encodes the raw buffers it writes with the shared
//! `bop-dataset` encoders. A protocol change must change both sides and the
//! protocol version together.
//!
//! Guide: <https://bobillumine.github.io/bop-dataset-gen/contributing/renderer-backends.html>

use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use bop_assets::{ResolvedAssetCatalog, Texture};
use bop_core::{
    BackendInfo, BackendKind, BlenderProcRendererConfig, BlenderProcSampling, NonNegativeFinite,
    RenderError, RenderReport, RenderSceneRequest, RenderStageTimings, RendererCapabilities,
};
use bop_dataset::{DepthFrame, InstanceFrame, RgbFrame, SceneStagingArea};
use bop_render_api::Renderer;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

/// Exact JSON-lines protocol version shared with the Python worker.
pub const PROTOCOL_VERSION: u16 = 1;
const PROTOCOL_REPLY_PREFIX: &str = "BOP1\t";

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
        /// Maximum Cycles samples per pixel.
        samples: u32,
        /// Adaptive-sampling noise threshold; zero disables it.
        noise_threshold: f32,
        /// RGB denoiser: `none`, `oidn` or `optix`.
        denoiser: String,
        /// Light bounces off glossy surfaces.
        glossy_bounces: u32,
        /// This worker's position among the run's renderer workers.
        #[serde(default)]
        worker_index: u32,
        /// The run's renderer workers; above one, each worker keeps a single
        /// GPU of the selected devices, `worker_index` modulo their count.
        #[serde(default = "one_worker")]
        worker_count: u32,
    },
}

const fn one_worker() -> u32 {
    1
}

impl WorkerMessage {
    /// Constructs the mandatory first protocol message for a run's only worker.
    #[must_use]
    pub fn hello(device_selection: impl Into<String>, sampling: BlenderProcSampling) -> Self {
        Self::hello_for_worker(device_selection, sampling, 0, 1)
    }

    /// Constructs the first protocol message for worker `worker_index` of
    /// `worker_count`.
    #[must_use]
    pub fn hello_for_worker(
        device_selection: impl Into<String>,
        sampling: BlenderProcSampling,
        worker_index: u32,
        worker_count: u32,
    ) -> Self {
        Self::Hello {
            protocol_version: ProtocolVersion,
            device_selection: device_selection.into(),
            samples: sampling.samples(),
            noise_threshold: sampling.noise_threshold(),
            denoiser: sampling.denoiser().name().to_owned(),
            glossy_bounces: sampling.glossy_bounces(),
            worker_index,
            worker_count,
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
        /// Controller request identity.
        request_id: u64,
    },
    /// Confirms raw output for one scene is complete.
    Rendered {
        /// Exact protocol compatibility guard.
        protocol_version: ProtocolVersion,
        /// Rendered scene identifier.
        scene_id: u64,
        /// Worker-reported render duration.
        render_seconds: f32,
        /// Controller request identity.
        request_id: u64,
    },
    /// Confirms all worker-side operations are durable.
    Flushed {
        /// Exact protocol compatibility guard.
        protocol_version: ProtocolVersion,
        /// Controller request identity.
        request_id: u64,
    },
    /// Reports a request-level worker failure without crashing the process.
    Error {
        /// Exact protocol compatibility guard.
        protocol_version: ProtocolVersion,
        /// Stable worker error category.
        code: String,
        /// Human-readable diagnostic.
        message: String,
        /// Controller request identity.
        request_id: u64,
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
    prepared_payload: Option<serde_json::Value>,
    next_request_id: u64,
    worker_index: u32,
    worker_count: u32,
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
        Self::try_new_for_worker(config, 0, 1)
    }

    /// Starts `BlenderProc` as renderer worker `worker_index` of
    /// `worker_count`. With more than one worker, each keeps one GPU of the
    /// selected devices, so the workers spread over the machine's GPUs.
    ///
    /// # Errors
    ///
    /// Returns a typed process, timeout, or protocol initialization error.
    pub fn try_new_for_worker(
        config: BlenderProcRendererConfig,
        worker_index: u32,
        worker_count: u32,
    ) -> Result<Self, BlenderProcInitializationError> {
        let mut worker = Worker::spawn(&config)?;
        worker.send(&WorkerMessage::hello_for_worker(
            config.device_selection(),
            config.sampling(),
            worker_index,
            worker_count,
        ))?;
        let ready = worker.receive(Duration::from_secs_f32(config.handshake_timeout_s()))?;
        let (implementation_version, device_name) = match ready {
            WorkerReply::Ready {
                implementation_version,
                device_name,
                ..
            } => (implementation_version, device_name),
            WorkerReply::Error { code, message, .. } => {
                return Err(BlenderProcInitializationError::Worker { code, message });
            }
            reply => {
                return Err(BlenderProcInitializationError::UnexpectedReply(reply_kind(
                    &reply,
                )));
            }
        };
        let sampling = config.sampling();
        let info = BackendInfo::try_new(
            BackendKind::BlenderProc,
            implementation_version,
            device_name,
            BTreeMap::from([
                ("protocol_version".to_owned(), PROTOCOL_VERSION.to_string()),
                ("samples".to_owned(), sampling.samples().to_string()),
                (
                    "noise_threshold".to_owned(),
                    sampling.noise_threshold().to_string(),
                ),
                ("denoiser".to_owned(), sampling.denoiser().name().to_owned()),
                (
                    "glossy_bounces".to_owned(),
                    sampling.glossy_bounces().to_string(),
                ),
            ]),
        )?;
        let channels = bop_core::RenderChannels::try_new(true, true, true)
            .map_err(|_| BlenderProcInitializationError::InternalInvariant)?;
        Ok(Self {
            config,
            info,
            capabilities: RendererCapabilities::new(channels, None),
            worker,
            prepared_fingerprint: None,
            prepared_payload: None,
            next_request_id: 0,
            worker_index,
            worker_count,
        })
    }

    /// Returns immutable worker provenance.
    #[must_use]
    pub fn info(&self) -> BackendInfo {
        self.info.clone()
    }
}

impl Renderer for BlenderProcRenderer {
    fn info(&self) -> BackendInfo {
        self.info()
    }

    fn capabilities(&self) -> RendererCapabilities {
        self.capabilities
    }

    fn prepare(&mut self, assets: &ResolvedAssetCatalog) -> Result<(), RenderError> {
        let request_id = self.allocate_request_id()?;
        let payload = json!({
            "protocol_version": PROTOCOL_VERSION,
            "type": "prepare",
            "request_id": request_id,
            "catalog_fingerprint": assets.fingerprint(),
            "assets": catalog_wire(assets),
            // Blender loads the same file itself; the absolute path keeps the
            // worker independent of its working directory.
            "environment_path": assets.environment().map(|map| {
                std::fs::canonicalize(map.source_path())
                    .unwrap_or_else(|_| map.source_path().to_owned())
            }),
        });
        self.worker
            .send(&payload)
            .map_err(|error| initialization_as_render(&error))?;
        match self.receive_request(request_id)? {
            WorkerReply::Prepared {
                catalog_fingerprint,
                ..
            } if catalog_fingerprint == assets.fingerprint() => {
                self.prepared_fingerprint = Some(catalog_fingerprint);
                self.prepared_payload = Some(payload);
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
        staging: &SceneStagingArea,
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
        let request_id = self.allocate_request_id()?;
        let payload = json!({
            "protocol_version": PROTOCOL_VERSION,
            "type": "render",
            "request_id": request_id,
            "request": request,
            "output_root": staging.root(),
        });
        self.worker
            .send(&payload)
            .map_err(|error| initialization_as_render(&error))?;
        let mut worker_restart_count = 0;
        let reply = match self.receive_request(request_id) {
            Ok(reply) => reply,
            Err(RenderError::WorkerCrashed { .. }) => {
                self.restart_and_reprepare()?;
                worker_restart_count = 1;
                self.worker
                    .send(&payload)
                    .map_err(|error| initialization_as_render(&error))?;
                self.receive_request(request_id)?
            }
            Err(error) => return Err(error),
        };
        let render_seconds = match reply {
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
            worker_restart_count,
        ))
    }

    fn flush(&mut self) -> Result<(), RenderError> {
        let request_id = self.allocate_request_id()?;
        self.worker
            .send(&json!({"protocol_version": PROTOCOL_VERSION, "type": "flush", "request_id": request_id}))
            .map_err(|error| initialization_as_render(&error))?;
        match self.receive_request(request_id)? {
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
    /// Rows top to bottom, as glTF stores them.
    rgba8_srgb: &'a [u8],
    /// Magnification samples the nearest texel instead of interpolating.
    nearest: bool,
    /// `repeat`, `mirror` or `clamp` along U and V.
    wrap: [&'static str; 2],
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
                        material: primitive.material().map(bop_assets::MaterialIndex::get),
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
    let wrap = |mode| match mode {
        bop_assets::WrapMode::Repeat => "repeat",
        bop_assets::WrapMode::MirroredRepeat => "mirror",
        bop_assets::WrapMode::ClampToEdge => "clamp",
    };
    let sampler = texture.sampler();
    TextureWire {
        dimensions: texture.image().dimensions(),
        rgba8_srgb: texture.image().rgba8_srgb(),
        nearest: sampler.magnification() == Some(bop_assets::MagnificationFilter::Nearest),
        wrap: [wrap(sampler.wrap_u()), wrap(sampler.wrap_v())],
    }
}

fn append_diagnostic(tail: &Mutex<VecDeque<u8>>, capacity: usize, bytes: &[u8]) {
    if let Ok(mut tail) = tail.lock() {
        for byte in bytes {
            if tail.len() == capacity {
                tail.pop_front();
            }
            tail.push_back(*byte);
        }
    }
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: mpsc::Receiver<Result<String, std::io::Error>>,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    diagnostic_capacity: usize,
}

/// Starts `command`, retrying briefly while its executable is busy.
///
/// On Linux, a file that was just written can stay open in a child another
/// thread forked meanwhile, until that child execs; executing it fails with
/// `ETXTBSY` in that window.
fn spawn_retrying_busy(command: &mut Command) -> std::io::Result<Child> {
    let mut attempts: u32 = 0;
    loop {
        match command.spawn() {
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 10 =>
            {
                attempts += 1;
                thread::sleep(Duration::from_millis(10 * u64::from(attempts)));
            }
            result => return result,
        }
    }
}

impl Worker {
    fn spawn(config: &BlenderProcRendererConfig) -> Result<Self, BlenderProcInitializationError> {
        let mut command = Command::new(config.executable_path());
        command
            .arg("run")
            .arg(config.worker_path())
            .arg("--")
            .arg("--bop-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // A terminal Ctrl-C goes to the whole foreground process group. Keep the
        // worker out of it so an interrupted run can finish its in-flight scene;
        // the worker still exits when its stdin closes.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child =
            spawn_retrying_busy(&mut command).map_err(BlenderProcInitializationError::Spawn)?;
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
                append_diagnostic(&stderr_capture, capacity, &[byte]);
            }
        });
        Ok(Self {
            child,
            stdin,
            stdout: receiver,
            stderr_tail,
            diagnostic_capacity: capacity,
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
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.stdout.recv_timeout(remaining) {
                Ok(Ok(line)) => {
                    if let Some(payload) = line.strip_prefix(PROTOCOL_REPLY_PREFIX) {
                        return serde_json::from_str(payload)
                            .map_err(BlenderProcInitializationError::Protocol);
                    }
                    append_diagnostic(
                        &self.stderr_tail,
                        self.diagnostic_capacity,
                        format!("{line}\n").as_bytes(),
                    );
                }
                Ok(Err(error)) => return Err(BlenderProcInitializationError::Read(error)),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(BlenderProcInitializationError::Timeout {
                        stderr_tail: self.stderr_tail(),
                    });
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(BlenderProcInitializationError::Exited {
                        status: self
                            .child
                            .try_wait()
                            .ok()
                            .flatten()
                            .map(|status| status.to_string()),
                        stderr_tail: self.stderr_tail(),
                    });
                }
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
        // `blenderproc run` starts Blender as its own child and passes on only
        // SIGTERM, so killing the launcher alone would leave Blender running.
        // The worker leads its own process group (see `spawn`); kill all of it.
        #[cfg(unix)]
        let _ = rustix::process::kill_process_group(
            rustix::process::Pid::from_child(&self.child),
            rustix::process::Signal::KILL,
        );
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl BlenderProcRenderer {
    fn allocate_request_id(&mut self) -> Result<u64, RenderError> {
        let request_id = self.next_request_id;
        self.next_request_id =
            self.next_request_id
                .checked_add(1)
                .ok_or_else(|| RenderError::Internal {
                    message: "BlenderProc request ID overflow".to_owned(),
                })?;
        Ok(request_id)
    }

    fn receive_request(&mut self, expected_request_id: u64) -> Result<WorkerReply, RenderError> {
        let reply = self
            .worker
            .receive(Duration::from_secs_f32(self.config.request_timeout_s()))
            .map_err(|error| initialization_as_render(&error))?;
        if reply_request_id(&reply) != Some(expected_request_id) {
            return Err(protocol_error(format!(
                "worker reply request ID {:?}; expected {expected_request_id}",
                reply_request_id(&reply)
            )));
        }
        Ok(reply)
    }

    fn restart_and_reprepare(&mut self) -> Result<(), RenderError> {
        let mut worker =
            Worker::spawn(&self.config).map_err(|error| initialization_as_render(&error))?;
        worker
            .send(&WorkerMessage::hello_for_worker(
                self.config.device_selection(),
                self.config.sampling(),
                self.worker_index,
                self.worker_count,
            ))
            .map_err(|error| initialization_as_render(&error))?;
        match worker
            .receive(Duration::from_secs_f32(self.config.handshake_timeout_s()))
            .map_err(|error| initialization_as_render(&error))?
        {
            WorkerReply::Ready { .. } => {}
            reply => {
                return Err(protocol_error(format!(
                    "restart expected ready, received {}",
                    reply_kind(&reply)
                )));
            }
        }
        let payload = self
            .prepared_payload
            .as_ref()
            .ok_or(RenderError::NotPrepared)?;
        let request_id = payload
            .get("request_id")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                protocol_error("stored prepare request omitted request_id".to_owned())
            })?;
        worker
            .send(payload)
            .map_err(|error| initialization_as_render(&error))?;
        let reply = worker
            .receive(Duration::from_secs_f32(self.config.request_timeout_s()))
            .map_err(|error| initialization_as_render(&error))?;
        if !matches!(&reply, WorkerReply::Prepared { request_id: actual, .. } if *actual == request_id)
        {
            return Err(protocol_error(format!(
                "restart prepare received {}",
                reply_kind(&reply)
            )));
        }
        self.worker = worker;
        Ok(())
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
    /// Worker startup returned a structured failure.
    #[error("BlenderProc worker failed during initialization ({code}): {message}")]
    Worker {
        /// Stable exception or error class.
        code: String,
        /// Worker-provided diagnostic.
        message: String,
    },
    /// Worker provenance violated the shared backend contract.
    #[error(transparent)]
    InvalidProvenance(#[from] bop_core::InvalidBackendInfoError),
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
        | BlenderProcInitializationError::UnexpectedReply(_)
        | BlenderProcInitializationError::Worker { .. } => RenderError::Protocol {
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

fn reply_request_id(reply: &WorkerReply) -> Option<u64> {
    match reply {
        WorkerReply::Ready { .. } => None,
        WorkerReply::Prepared { request_id, .. }
        | WorkerReply::Rendered { request_id, .. }
        | WorkerReply::Flushed { request_id, .. }
        | WorkerReply::Error { request_id, .. } => Some(*request_id),
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
                .write_png(
                    &staging.rgb_path(camera.camera_id()),
                    request.png_compression(),
                )
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
                .write_png(
                    &staging.instance_path(camera.camera_id()),
                    request.png_compression(),
                )
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
