/// Selected compile-time contract for Rapier numeric behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicsBuildProfile {
    /// Stable eight-lane SIMD for maximum same-machine throughput.
    Fast,
    /// Scalar enhanced-determinism math for replay-focused builds.
    Reproducible,
}

/// Returns the exactly one physics profile selected at compile time.
#[must_use]
pub const fn physics_build_profile() -> PhysicsBuildProfile {
    #[cfg(feature = "fast")]
    {
        PhysicsBuildProfile::Fast
    }
    #[cfg(feature = "reproducible")]
    {
        PhysicsBuildProfile::Reproducible
    }
}
