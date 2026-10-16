//! Ring-buffered GPU-to-CPU frame readback and timestamp mapping.

#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the crate root's imports and helpers"
)]
use super::*;

pub(crate) struct ReadbackFrame {
    pub(crate) rgba8: Vec<u8>,
    pub(crate) depth: Vec<f32>,
    pub(crate) instance: Vec<u16>,
    pub(crate) gpu_seconds: Option<f32>,
}

pub(crate) struct PendingReadback {
    pub(crate) submission: wgpu::SubmissionIndex,
    pub(crate) rgb: ReadbackBuffer,
    pub(crate) depth: ReadbackBuffer,
    pub(crate) instance: ReadbackBuffer,
    pub(crate) rgb_mapping: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    pub(crate) depth_mapping: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    pub(crate) instance_mapping: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    pub(crate) timestamp: Option<PendingTimestamp>,
}

pub(crate) struct PendingTimestamp {
    pub(crate) buffer: wgpu::Buffer,
    pub(crate) mapping: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    pub(crate) period_ns: f32,
}

pub(crate) const READBACK_RING_CAPACITY: usize = 3;

impl PendingReadback {
    pub(crate) fn finish(self, device: &wgpu::Device) -> Result<ReadbackFrame, WgpuReadbackError> {
        device.poll(wgpu::PollType::Wait {
            submission_index: Some(self.submission),
            timeout: Some(Duration::from_secs(30)),
        })?;
        let rgba8 = self.rgb.finish_read(&self.rgb_mapping)?;
        let depth_bytes = self.depth.finish_read(&self.depth_mapping)?;
        let instance_bytes = self.instance.finish_read(&self.instance_mapping)?;
        let depth = depth_bytes
            .chunks_exact(4)
            .map(|bytes| f32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect();
        let instance = instance_bytes
            .chunks_exact(4)
            .map(|bytes| u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .map(|value| {
                u16::try_from(value).map_err(|_| WgpuReadbackError::InstanceIdOverflow { value })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let gpu_seconds = self.timestamp.map(PendingTimestamp::finish).transpose()?;
        Ok(ReadbackFrame {
            rgba8,
            depth,
            instance,
            gpu_seconds,
        })
    }
}

impl PendingTimestamp {
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    pub(crate) fn finish(self) -> Result<f32, WgpuReadbackError> {
        receive_mapping(&self.mapping).map_err(map_mapping_completion)?;
        let mapped = self.buffer.slice(..).get_mapped_range()?;
        let timestamps: &[u64] = bytemuck::cast_slice(&mapped);
        let ticks = timestamps[1].saturating_sub(timestamps[0]);
        let seconds = (ticks as f64 * f64::from(self.period_ns) * 1.0e-9) as f32;
        drop(mapped);
        self.buffer.unmap();
        if seconds.is_finite() {
            Ok(seconds)
        } else {
            Err(WgpuReadbackError::TimestampOverflow)
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum MappingCompletionError<E> {
    CallbackDropped,
    Mapping(E),
}

pub(crate) fn receive_mapping<E>(
    receiver: &mpsc::Receiver<Result<(), E>>,
) -> Result<(), MappingCompletionError<E>> {
    receiver
        .recv()
        .map_err(|_| MappingCompletionError::CallbackDropped)?
        .map_err(MappingCompletionError::Mapping)
}

pub(crate) fn map_mapping_completion(
    error: MappingCompletionError<wgpu::BufferAsyncError>,
) -> WgpuReadbackError {
    match error {
        MappingCompletionError::CallbackDropped => WgpuReadbackError::CallbackDropped,
        MappingCompletionError::Mapping(error) => WgpuReadbackError::Map(error),
    }
}

pub(crate) struct ReadbackBuffer {
    pub(crate) buffer: wgpu::Buffer,
    pub(crate) padded_bytes_per_row: u32,
    pub(crate) unpadded_bytes_per_row: u32,
    pub(crate) height: u32,
}

impl ReadbackBuffer {
    pub(crate) fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        label: &str,
    ) -> Result<Self, WgpuReadbackError> {
        let unpadded_bytes_per_row = width
            .checked_mul(bytes_per_pixel)
            .ok_or(WgpuReadbackError::SizeOverflow)?;
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row
            .checked_add(alignment - 1)
            .map(|value| value / alignment * alignment)
            .ok_or(WgpuReadbackError::SizeOverflow)?;
        let size = u64::from(padded_bytes_per_row)
            .checked_mul(u64::from(height))
            .ok_or(WgpuReadbackError::SizeOverflow)?;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Ok(Self {
            buffer,
            padded_bytes_per_row,
            unpadded_bytes_per_row,
            height,
        })
    }

    pub(crate) fn copy_from(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        extent: wgpu::Extent3d,
    ) {
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            extent,
        );
    }

    pub(crate) fn begin_read(&self) -> mpsc::Receiver<Result<(), wgpu::BufferAsyncError>> {
        let slice = self.buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        receiver
    }

    pub(crate) fn finish_read(
        &self,
        receiver: &mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    ) -> Result<Vec<u8>, WgpuReadbackError> {
        let slice = self.buffer.slice(..);
        receive_mapping(receiver).map_err(map_mapping_completion)?;
        let mapped = slice.get_mapped_range()?;
        let capacity = usize::try_from(self.unpadded_bytes_per_row)
            .ok()
            .and_then(|row| {
                usize::try_from(self.height)
                    .ok()
                    .and_then(|height| row.checked_mul(height))
            })
            .ok_or(WgpuReadbackError::SizeOverflow)?;
        let mut bytes = Vec::with_capacity(capacity);
        let padded = usize::try_from(self.padded_bytes_per_row)
            .map_err(|_| WgpuReadbackError::SizeOverflow)?;
        let unpadded = usize::try_from(self.unpadded_bytes_per_row)
            .map_err(|_| WgpuReadbackError::SizeOverflow)?;
        for row in mapped.chunks_exact(padded) {
            bytes.extend_from_slice(&row[..unpadded]);
        }
        drop(mapped);
        self.buffer.unmap();
        Ok(bytes)
    }
}

/// Failure during bounded GPU readback.
#[derive(Debug, Error)]
pub(crate) enum WgpuReadbackError {
    #[error("render target or readback size overflow")]
    SizeOverflow,
    #[error("GPU polling failed: {0}")]
    Poll(#[from] wgpu::PollError),
    #[error("GPU buffer mapping failed: {0}")]
    Map(#[from] wgpu::BufferAsyncError),
    #[error("GPU mapped range access failed: {0}")]
    MapRange(#[from] wgpu::MapRangeError),
    #[error("GPU mapping callback was dropped")]
    CallbackDropped,
    #[error("GPU instance target returned ID {value} outside the canonical u16 domain")]
    InstanceIdOverflow { value: u32 },
    #[error("GPU timestamp duration exceeded the finite f32 report domain")]
    TimestampOverflow,
}
