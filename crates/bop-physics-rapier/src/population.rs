use std::collections::{BTreeMap, BTreeSet};

use bop_core::{
    AssetId, Distribution, InstanceId, InvalidResolvedPhysicalPropertiesError,
    PhysicalPropertyDistributions, ResolvedPhysicalProperties, RunPlan,
};
use thiserror::Error;

use crate::sampling::{DeterministicSampler, Field};

/// Immutable, deterministic compiler for one run plan's object populations.
///
/// Construction copies only the population distributions and selected asset
/// physical-property distributions. In particular, legacy
/// [`bop_core::AssetConfig::mixture_weight`] is accepted by configuration
/// validation but intentionally ignored here: `population.asset_mixture` is
/// the sole authority for asset selection.
#[derive(Clone, Debug)]
pub struct PopulationPlanner {
    bins: Vec<BinPopulation>,
    assets: BTreeMap<AssetId, PhysicalPropertyDistributions>,
}

#[derive(Clone, Debug)]
struct BinPopulation {
    object_count: Distribution<u32>,
    asset_mixture: Distribution<AssetId>,
}

impl PopulationPlanner {
    /// Compiles the validated data needed to plan object populations.
    ///
    /// # Errors
    ///
    /// Returns [`PopulationPlanningError::MissingAssetReference`] if the run
    /// plan has an asset mixture reference absent from its asset catalog.
    pub fn try_new(plan: &RunPlan) -> Result<Self, PopulationPlanningError> {
        let configured_asset_ids = plan
            .assets()
            .iter()
            .map(bop_core::AssetConfig::id)
            .collect::<BTreeSet<_>>();
        let mut selectable = BTreeSet::new();
        let mut bins = Vec::with_capacity(plan.bins().len());
        for bin in plan.bins() {
            let asset_mixture = bin.population().asset_mixture().clone();
            for asset_id in distribution_asset_ids(&asset_mixture) {
                if !configured_asset_ids.contains(asset_id) {
                    return Err(PopulationPlanningError::MissingAssetReference {
                        asset_id: asset_id.clone(),
                    });
                }
            }
            selectable.extend(selectable_asset_ids(&asset_mixture).into_iter().cloned());
            bins.push(BinPopulation {
                object_count: bin.population().object_count().clone(),
                asset_mixture,
            });
        }
        let assets = plan
            .assets()
            .iter()
            .filter(|asset| selectable.contains(asset.id()))
            .map(|asset| (asset.id().clone(), asset.physical_properties().clone()))
            .collect::<BTreeMap<_, _>>();
        Ok(Self { bins, assets })
    }

    /// Resolves a population using the supplied already-derived population seed.
    ///
    /// Bins are filled in configuration order and instance IDs continue across
    /// them, so the first bin's objects are exactly those a single-bin
    /// configuration would produce.
    ///
    /// # Errors
    ///
    /// Returns a typed planning error for an unrepresentable object count, a
    /// missing asset reference, or resolved physical properties outside their
    /// backend-neutral domain.
    pub fn plan(
        &self,
        population_seed: [u8; 32],
    ) -> Result<PopulationPlan, PopulationPlanningError> {
        let sampler = DeterministicSampler::new(population_seed);
        let counts = self
            .bins
            .iter()
            .zip(0_u32..)
            .map(|(bin, bin_index)| sampler.u32(&bin.object_count, Field::ObjectCount, bin_index))
            .collect::<Vec<_>>();
        let object_count = counts.iter().copied().fold(0_u32, u32::saturating_add);
        if object_count > u32::from(u16::MAX) {
            return Err(
                PopulationPlanningError::ObjectCountExceedsInstanceIdCapacity {
                    count: object_count,
                },
            );
        }
        let bin_of_instance = counts
            .iter()
            .enumerate()
            .flat_map(|(bin_index, &count)| std::iter::repeat_n(bin_index, count as usize));
        let mut instances = Vec::with_capacity(object_count as usize);
        for (raw_instance_id, bin_index) in (1..=object_count).zip(bin_of_instance) {
            let instance_id = InstanceId::new(u16::try_from(raw_instance_id).map_err(|_| {
                PopulationPlanningError::ObjectCountExceedsInstanceIdCapacity {
                    count: object_count,
                }
            })?)
            .ok_or(
                PopulationPlanningError::ObjectCountExceedsInstanceIdCapacity {
                    count: object_count,
                },
            )?;
            let asset_id = sampler
                .choice(
                    &self.bins[bin_index].asset_mixture,
                    Field::Asset,
                    raw_instance_id,
                )
                .clone();
            let properties = self.assets.get(&asset_id).ok_or_else(|| {
                PopulationPlanningError::MissingAssetReference {
                    asset_id: asset_id.clone(),
                }
            })?;
            let physical_properties = resolve_properties(
                &sampler,
                properties,
                raw_instance_id,
                instance_id,
                &asset_id,
            )?;
            instances.push(PlannedInstance {
                instance_id,
                asset_id,
                bin_index,
                physical_properties,
            });
        }
        Ok(PopulationPlan { instances })
    }
}

/// A fully resolved, identity-ordered population for one scene.
#[derive(Clone, Debug, PartialEq)]
pub struct PopulationPlan {
    instances: Vec<PlannedInstance>,
}

impl PopulationPlan {
    /// Borrows planned instances in ascending contiguous identity order.
    #[must_use]
    pub fn instances(&self) -> &[PlannedInstance] {
        &self.instances
    }

    /// Consumes this plan and returns its planned instances.
    #[must_use]
    pub fn into_instances(self) -> Vec<PlannedInstance> {
        self.instances
    }
}

/// One asset and fully resolved physical-property tuple selected for an instance.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedInstance {
    instance_id: InstanceId,
    asset_id: AssetId,
    bin_index: usize,
    physical_properties: ResolvedPhysicalProperties,
}

impl PlannedInstance {
    /// Returns the stable foreground identity of this planned instance.
    #[must_use]
    pub const fn instance_id(&self) -> InstanceId {
        self.instance_id
    }

    /// Returns the asset selected exclusively from the population mixture.
    #[must_use]
    pub const fn asset_id(&self) -> &AssetId {
        &self.asset_id
    }

    /// Returns the index into [`RunPlan::bins`] of the bin this instance is dropped into.
    #[must_use]
    pub const fn bin_index(&self) -> usize {
        self.bin_index
    }

    /// Returns the physical properties resolved for the selected asset.
    #[must_use]
    pub const fn physical_properties(&self) -> ResolvedPhysicalProperties {
        self.physical_properties
    }
}

/// Error returned while compiling or resolving a population plan.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum PopulationPlanningError {
    /// Sampling requested more foreground identities than the `u16` encoding permits.
    #[error("object count {count} exceeds the InstanceId capacity of 65535")]
    ObjectCountExceedsInstanceIdCapacity {
        /// The sampled object count.
        count: u32,
    },
    /// A population mixture refers to an asset absent from the compiled catalog.
    #[error("population mixture refers to missing asset {asset_id:?}")]
    MissingAssetReference {
        /// The missing logical asset identifier.
        asset_id: AssetId,
    },
    /// Sampling produced physical properties that violate the resolved domain.
    #[error(
        "resolved physical properties for instance {instance_id:?}, asset {asset_id:?} are invalid: {source}"
    )]
    InvalidResolvedPhysicalProperties {
        /// The affected foreground instance identity.
        instance_id: InstanceId,
        /// The selected logical asset identifier.
        asset_id: AssetId,
        /// The rejected resolved-property invariant.
        #[source]
        source: InvalidResolvedPhysicalPropertiesError,
    },
}

fn distribution_asset_ids(distribution: &Distribution<AssetId>) -> Vec<&AssetId> {
    match distribution.view() {
        bop_core::DistributionView::Fixed { value } => vec![value],
        bop_core::DistributionView::Choice { values } => {
            values.iter().map(bop_core::WeightedValue::value).collect()
        }
        bop_core::DistributionView::Uniform { .. } => {
            unreachable!("asset distributions are checked without uniform variants")
        }
    }
}

fn selectable_asset_ids(distribution: &Distribution<AssetId>) -> BTreeSet<&AssetId> {
    match distribution.view() {
        bop_core::DistributionView::Fixed { value } => BTreeSet::from([value]),
        bop_core::DistributionView::Choice { values } => values
            .iter()
            .filter(|value| value.weight() > 0.0)
            .map(bop_core::WeightedValue::value)
            .collect(),
        bop_core::DistributionView::Uniform { .. } => {
            unreachable!("asset distributions are checked without uniform variants")
        }
    }
}

fn resolve_properties(
    sampler: &DeterministicSampler,
    distributions: &PhysicalPropertyDistributions,
    raw_instance_id: u32,
    instance_id: InstanceId,
    asset_id: &AssetId,
) -> Result<ResolvedPhysicalProperties, PopulationPlanningError> {
    let mass_kg = sampler.f32(distributions.mass_kg(), Field::MassKg, raw_instance_id);
    let friction = sampler.f32(distributions.friction(), Field::Friction, raw_instance_id);
    let restitution = sampler.f32(
        distributions.restitution(),
        Field::Restitution,
        raw_instance_id,
    );
    ResolvedPhysicalProperties::try_new(mass_kg, friction, restitution).map_err(|source| {
        PopulationPlanningError::InvalidResolvedPhysicalProperties {
            instance_id,
            asset_id: asset_id.clone(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use bop_core::{AssetId, Distribution, WeightedValue};

    use super::selectable_asset_ids;

    #[test]
    fn only_positive_population_mixture_entries_are_selectable() {
        let alpha = AssetId::new("alpha").expect("valid asset ID");
        let bravo = AssetId::new("bravo").expect("valid asset ID");
        let mixture = Distribution::<AssetId>::try_choice(vec![
            WeightedValue::try_new(alpha, 0.0).expect("valid zero weight"),
            WeightedValue::try_new(bravo, 1.0).expect("valid positive weight"),
        ])
        .expect("valid mixture");

        let selectable = selectable_asset_ids(&mixture);

        assert_eq!(
            selectable
                .into_iter()
                .map(AssetId::as_str)
                .collect::<Vec<_>>(),
            vec!["bravo"]
        );
    }
}
