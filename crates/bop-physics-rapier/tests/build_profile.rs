//! Public deterministic physics build-profile contract.

use bop_physics_rapier::{PhysicsBuildProfile, physics_build_profile};

#[test]
fn reports_the_single_selected_physics_build_profile() {
    #[cfg(feature = "fast")]
    assert_eq!(physics_build_profile(), PhysicsBuildProfile::Fast);

    #[cfg(feature = "reproducible")]
    assert_eq!(physics_build_profile(), PhysicsBuildProfile::Reproducible);
}
