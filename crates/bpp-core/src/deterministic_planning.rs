use std::{iter::FusedIterator, num::NonZeroU32, num::NonZeroU64};

use thiserror::Error;

use crate::{AttemptId, SceneId};

/// Independent deterministic stream used while constructing a scene.
///
/// Each variant has a fixed v1 byte label exposed by
/// [`SeedStage::protocol_label`]. The labels are protocol data: do not derive
/// them from `Debug`, `Display`, an enum discriminant, or a declaration order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SeedStage {
    /// Population sampling, labeled `bpp.dataset.seed-stage.v1/population`.
    Population,
    /// Object placement, labeled `bpp.dataset.seed-stage.v1/placement`.
    Placement,
    /// Camera sampling, labeled `bpp.dataset.seed-stage.v1/camera`.
    Camera,
    /// Appearance sampling, labeled `bpp.dataset.seed-stage.v1/appearance`.
    Appearance,
}

impl SeedStage {
    /// Returns this stage's fixed, versioned protocol label.
    ///
    /// These bytes are stable input to seed derivation. Changing them changes
    /// every derived stream for the affected stage and therefore requires an
    /// explicit protocol version change.
    #[must_use]
    pub const fn protocol_label(self) -> &'static [u8] {
        match self {
            Self::Population => b"bpp.dataset.seed-stage.v1/population",
            Self::Placement => b"bpp.dataset.seed-stage.v1/placement",
            Self::Camera => b"bpp.dataset.seed-stage.v1/camera",
            Self::Appearance => b"bpp.dataset.seed-stage.v1/appearance",
        }
    }
}

/// Immutable owner of private material for deterministic stream derivation.
///
/// # LEARN-01 protocol
///
/// Construction encodes the `u64` master seed explicitly as eight
/// little-endian bytes and passes those bytes to BLAKE3's key-derivation API
/// with context string `bpp.dataset.seed-master.v1`. The resulting 32 owned
/// bytes are the private keyed-hash material stored by this value.
///
/// A stream input is exactly the scene identifier as eight little-endian
/// bytes, followed by the attempt identifier as four little-endian bytes,
/// followed by [`SeedStage::protocol_label`]. Hash that input with keyed
/// BLAKE3 and return its 32 output bytes. Fixed-width identifiers make the
/// framing unambiguous without string conversion or delimiters.
///
/// Identical `(master seed, scene, attempt, stage)` tuples must reproduce the
/// same output, while changing any tuple component must select a different
/// stream. Native-endian encoding, debug formatting, string concatenation,
/// `DefaultHasher`, and a global RNG are not protocol-compatible substitutes.
/// Consult BLAKE3's official `derive_key` and keyed-hashing documentation while
/// implementing the exercise; the known vectors live in `tests/learn_01.rs`.
///
/// The value stays immutable after construction. [`SeedDeriver::derive`] takes
/// `&self`, advances no RNG cursor, and needs no mutex, so scheduling order
/// cannot affect its output. Owned bytes make the type naturally `Send + Sync`;
/// do not add `Arc`, `Mutex`, atomics, Rayon, or an RNG to this core contract.
pub struct SeedDeriver {
    #[allow(
        dead_code,
        reason = "the learner-owned derive body reads this private material after LEARN-01"
    )]
    key_material: [u8; 32],
}

impl SeedDeriver {
    /// Creates private deterministic key material from a master seed.
    ///
    /// This is a LEARN-01 exercise. Follow the documented KDF context and
    /// explicit little-endian encoding; keep the derived material private.
    ///
    /// # Panics
    ///
    /// The learning scaffold intentionally panics until LEARN-01 is completed.
    #[must_use]
    pub fn new(_master_seed: u64) -> Self {
        todo!("LEARN-01: derive private BLAKE3 key material from the master seed")
    }

    /// Derives the deterministic 32-byte stream seed for one tuple.
    ///
    /// This is a LEARN-01 exercise. Use the fixed-width byte framing and stage
    /// label documented on [`SeedDeriver`], without mutable or global state.
    ///
    /// # Panics
    ///
    /// The learning scaffold intentionally panics until LEARN-01 is completed.
    #[must_use]
    pub fn derive(&self, _scene: SceneId, _attempt: AttemptId, _stage: SeedStage) -> [u8; 32] {
        todo!("LEARN-01: derive a keyed BLAKE3 stream seed for the requested tuple")
    }
}

/// Immutable description of one scene-generation attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SceneJob {
    scene_id: SceneId,
    attempt_id: AttemptId,
}

impl SceneJob {
    /// Creates a job for one scene and attempt identifier.
    #[must_use]
    pub const fn new(scene_id: SceneId, attempt_id: AttemptId) -> Self {
        Self {
            scene_id,
            attempt_id,
        }
    }

    /// Returns the requested scene identifier.
    #[must_use]
    pub const fn scene_id(self) -> SceneId {
        self.scene_id
    }

    /// Returns the zero-based attempt identifier for this scene.
    #[must_use]
    pub const fn attempt_id(self) -> AttemptId {
        self.attempt_id
    }
}

/// Deterministic scene-major iterator over requested generation attempts.
///
/// The order is lexicographic by `(scene ID, attempt ID)`: scene IDs cover
/// `0..requested_scene_count`, and each scene emits attempt IDs covering
/// `0..attempt_limit` before the next scene begins.
///
/// # LEARN-01 iterator guidance
///
/// Before coding [`Iterator::next`], write a state-machine table covering an
/// ordinary attempt, the last attempt of a scene, the last scene, and the
/// already-exhausted state. For each row, check the emitted job, the next
/// counter state, and whether later calls must return `None`. Advance attempt
/// before scene, avoid recursion, and make exhaustion permanent.
///
/// The validated bounds and counters use the full `u64` scene and `u32`
/// attempt domains. The respective bound value is a representable exhausted
/// sentinel, so core iteration needs no total-length multiplication that could
/// overflow `usize`. Treat `size_hint` or `ExactSizeIterator` as optional
/// stretch work only after the core iterator is correct.
///
/// Iterator instances are worker-local planning state. Move jobs to workers;
/// do not share an iterator behind a lock. Scheduling must not change the
/// sequence a fresh iterator produces.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "the learner-owned Iterator::next body reads this state after LEARN-01"
)]
pub struct ScenePlanIter {
    requested_scene_count: NonZeroU64,
    attempt_limit: NonZeroU32,
    next_scene: u64,
    next_attempt: u32,
}

impl ScenePlanIter {
    /// Creates a scene plan from nonzero scene and attempt bounds.
    ///
    /// Construction stores only validated bounds and initial counters. It does
    /// not allocate every job or multiply the bounds into a total length.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidScenePlanError`] when either bound is zero.
    pub fn try_new(
        requested_scene_count: u64,
        attempt_limit: u32,
    ) -> Result<Self, InvalidScenePlanError> {
        let requested_scene_count = NonZeroU64::new(requested_scene_count)
            .ok_or(InvalidScenePlanError::ZeroRequestedSceneCount)?;
        let attempt_limit =
            NonZeroU32::new(attempt_limit).ok_or(InvalidScenePlanError::ZeroAttemptLimit)?;
        Ok(Self {
            requested_scene_count,
            attempt_limit,
            next_scene: 0,
            next_attempt: 0,
        })
    }
}

impl Iterator for ScenePlanIter {
    type Item = SceneJob;

    fn next(&mut self) -> Option<Self::Item> {
        todo!("LEARN-01: advance the scene-major iterator state machine")
    }
}

impl FusedIterator for ScenePlanIter {}

/// Error returned when a scene plan has an empty work domain.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidScenePlanError {
    /// The requested scene count was zero.
    #[error("requested scene count must be nonzero")]
    ZeroRequestedSceneCount,
    /// The per-scene attempt limit was zero.
    #[error("attempt limit must be nonzero")]
    ZeroAttemptLimit,
}
