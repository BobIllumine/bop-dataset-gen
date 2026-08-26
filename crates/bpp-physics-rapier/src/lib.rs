//! Rapier-based rigid-body scene generation.

mod prepared_asset;
mod world;

pub use prepared_asset::{PhysicsAssetPreparationError, PreparedPhysicsAsset};
pub use world::{DynamicBodySpec, PhysicsWorld, PhysicsWorldError};
