//! Rapier-based rigid-body scene generation.

mod generator;
mod placement;
mod placement_sampling;
mod population;
mod prepared_asset;
mod sampling;
mod world;

pub use generator::{SceneGenerationError, SerialPhysicsGenerator};
pub use placement::{PlacedInstance, PlacementPlan, PlacementPlanner, PlacementPlanningError};
pub use population::{PlannedInstance, PopulationPlan, PopulationPlanner, PopulationPlanningError};
pub use prepared_asset::{PhysicsAssetPreparationError, PreparedPhysicsAsset};
pub use world::{
    DynamicBodySpec, FinalStateMetrics, FinalStateValidationError, PhysicsWorld, PhysicsWorldError,
    SettlingOutcome,
};
