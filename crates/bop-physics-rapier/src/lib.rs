//! Rapier-based rigid-body scene generation.

#[cfg(all(feature = "fast", feature = "reproducible"))]
compile_error!("physics features `fast` and `reproducible` are mutually exclusive");
#[cfg(not(any(feature = "fast", feature = "reproducible")))]
compile_error!("select exactly one physics feature: `fast` or `reproducible`");

mod build_profile;
mod generator;
mod placement;
mod placement_sampling;
mod population;
mod prepared_asset;
mod sampling;
mod world;

pub use build_profile::{PhysicsBuildProfile, physics_build_profile};
pub use generator::{SceneGenerationError, SerialPhysicsGenerator};
pub use placement::{PlacedInstance, PlacementPlan, PlacementPlanner, PlacementPlanningError};
pub use population::{PlannedInstance, PopulationPlan, PopulationPlanner, PopulationPlanningError};
pub use prepared_asset::{PhysicsAssetPreparationError, PreparedPhysicsAsset};
pub use world::{
    DynamicBodySpec, FinalStateMetrics, FinalStateValidationError, PhysicsWorld, PhysicsWorldError,
    SettlingOutcome,
};
