use blake3::Hasher;
use bop_core::{Distribution, DistributionView, WeightedValue};

#[derive(Clone, Copy)]
pub(super) enum Field {
    ObjectCount,
    Asset,
    MassKg,
    Friction,
    Restitution,
}

impl Field {
    const fn label(self) -> &'static [u8] {
        match self {
            Self::ObjectCount => b"bpp.dataset.population-sampling.v1/object-count",
            Self::Asset => b"bpp.dataset.population-sampling.v1/asset",
            Self::MassKg => b"bpp.dataset.population-sampling.v1/mass-kg",
            Self::Friction => b"bpp.dataset.population-sampling.v1/friction",
            Self::Restitution => b"bpp.dataset.population-sampling.v1/restitution",
        }
    }
}

pub(super) struct DeterministicSampler {
    key: [u8; 32],
}

impl DeterministicSampler {
    pub(super) const fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub(super) fn u32(&self, distribution: &Distribution<u32>, field: Field, instance: u32) -> u32 {
        match distribution.view() {
            DistributionView::Fixed { value } => *value,
            DistributionView::Uniform { min, max, .. } => {
                self.uniform_u32(*min, *max, field, instance)
            }
            DistributionView::Choice { values } => *self.choose(values, field, instance),
        }
    }

    pub(super) fn f32(&self, distribution: &Distribution<f32>, field: Field, instance: u32) -> f32 {
        match distribution.view() {
            DistributionView::Fixed { value } => *value,
            DistributionView::Uniform { min, max, .. } if min.to_bits() == max.to_bits() => *min,
            DistributionView::Uniform { min, max, .. } => {
                self.uniform_f32(*min, *max, field, instance)
            }
            DistributionView::Choice { values } => *self.choose(values, field, instance),
        }
    }

    pub(super) fn choice<'a, T>(
        &self,
        distribution: &'a Distribution<T>,
        field: Field,
        instance: u32,
    ) -> &'a T {
        match distribution.view() {
            DistributionView::Fixed { value } => value,
            DistributionView::Choice { values } => self.choose(values, field, instance),
            DistributionView::Uniform { .. } => {
                unreachable!("asset distributions are checked without uniform variants")
            }
        }
    }

    fn uniform_u32(&self, min: u32, max: u32, field: Field, instance: u32) -> u32 {
        uniform_u32_with_bits(min, max, |rejection| self.bits(field, instance, rejection))
    }

    fn uniform_f32(&self, min: f32, max: f32, field: Field, instance: u32) -> f32 {
        uniform_f32_from_bits(min, max, self.bits(field, instance, 0))
    }

    fn choose<'a, T>(&self, values: &'a [WeightedValue<T>], field: Field, instance: u32) -> &'a T {
        let largest = values
            .iter()
            .map(WeightedValue::weight)
            .fold(0.0_f64, f64::max);
        let total = values
            .iter()
            .map(|value| value.weight() / largest)
            .sum::<f64>();
        let target = unit_interval(self.bits(field, instance, 0)) * total;
        let mut cumulative = 0.0;
        let mut last_positive = None;
        for value in values {
            let normalized = value.weight() / largest;
            if normalized > 0.0 {
                cumulative += normalized;
                last_positive = Some(value);
                if target < cumulative {
                    return value.value();
                }
            }
        }
        last_positive
            .expect("checked choice distributions contain a positive weight")
            .value()
    }

    fn bits(&self, field: Field, instance: u32, rejection: u32) -> u64 {
        let mut hasher = Hasher::new_keyed(&self.key);
        hasher.update(field.label());
        hasher.update(&instance.to_le_bytes());
        hasher.update(&rejection.to_le_bytes());
        let digest = hasher.finalize();
        u64::from_le_bytes(
            digest.as_bytes()[..8]
                .try_into()
                .expect("digest has eight bytes"),
        )
    }
}

fn uniform_u32_with_bits(min: u32, max: u32, mut bits: impl FnMut(u32) -> u64) -> u32 {
    let range = u64::from(max) - u64::from(min) + 1;
    let threshold = range.wrapping_neg() % range;
    let mut rejection = 0;
    loop {
        let product = u128::from(bits(rejection)) * u128::from(range);
        let low =
            u64::try_from(product & u128::from(u64::MAX)).expect("the masked product fits in u64");
        if low >= threshold {
            let offset = u32::try_from(product >> 64)
                .expect("a u32 range makes the high product half fit in u32");
            return min + offset;
        }
        rejection += 1;
    }
}

fn uniform_f32_from_bits(min: f32, max: f32, bits: u64) -> f32 {
    let unit = unit_interval(bits);
    #[allow(clippy::cast_possible_truncation)]
    let sampled = (f64::from(min) + (f64::from(max) - f64::from(min)) * unit) as f32;
    if sampled >= max {
        next_down(max)
    } else {
        sampled
    }
}

fn unit_interval(bits: u64) -> f64 {
    const SCALE: f64 = 1.0 / 9_007_199_254_740_992.0;
    let numerator = bits >> 11;
    let high = u32::try_from(numerator >> 21).expect("53-bit numerator has a 32-bit high half");
    let low =
        u32::try_from(numerator & ((1 << 21) - 1)).expect("53-bit numerator has a 21-bit low half");
    (f64::from(high) * 2_097_152.0 + f64::from(low)) * SCALE
}

fn next_down(value: f32) -> f32 {
    debug_assert!(value.is_finite());
    if value == 0.0 {
        -f32::from_bits(1)
    } else if value.is_sign_positive() {
        f32::from_bits(value.to_bits() - 1)
    } else {
        f32::from_bits(value.to_bits() + 1)
    }
}

#[cfg(test)]
mod tests {
    use bop_core::{Distribution, WeightedValue};

    use super::{
        DeterministicSampler, Field, next_down, uniform_f32_from_bits, uniform_u32_with_bits,
    };

    #[test]
    fn object_count_address_has_a_locked_label_and_nonzero_rejection_counter() {
        let sampler = DeterministicSampler::new([0x5a; 32]);

        assert_eq!(
            sampler.bits(Field::ObjectCount, 0, 0),
            8_935_015_796_129_262_327
        );
        assert_eq!(
            sampler.bits(Field::ObjectCount, 0, 1),
            7_735_373_043_089_409_088
        );
    }

    #[test]
    fn uniform_object_count_has_a_locked_protocol_vector() {
        let sampler = DeterministicSampler::new([0x5a; 32]);
        let distribution = Distribution::<u32>::try_uniform(2, 7).expect("checked bounds");

        assert_eq!(sampler.u32(&distribution, Field::ObjectCount, 0), 4);
    }

    #[test]
    fn integer_uniform_retries_only_the_rejected_address_counter() {
        let mut seen_rejections = Vec::new();

        let value = uniform_u32_with_bits(10, 12, |rejection| {
            seen_rejections.push(rejection);
            if rejection == 0 { 0 } else { u64::MAX }
        });

        assert_eq!(seen_rejections, vec![0, 1]);
        assert_eq!(value, 12);
    }

    #[test]
    fn integer_uniform_includes_both_bounds() {
        let sampler = DeterministicSampler::new([1; 32]);
        let distribution = Distribution::<u32>::try_uniform(7, 8).expect("checked bounds");
        let values = (0..128)
            .map(|instance| sampler.u32(&distribution, Field::ObjectCount, instance))
            .collect::<Vec<_>>();

        assert!(values.contains(&7));
        assert!(values.contains(&8));
    }

    #[test]
    fn weighted_selection_never_selects_zero_weight_values() {
        let sampler = DeterministicSampler::new([2; 32]);
        let distribution = Distribution::<u32>::try_choice(vec![
            WeightedValue::try_new(1_u32, 0.0).expect("weight is valid"),
            WeightedValue::try_new(2_u32, f64::MAX).expect("weight is valid"),
        ])
        .expect("choice has a positive weight");

        for instance in 0..128 {
            assert_eq!(sampler.u32(&distribution, Field::ObjectCount, instance), 2);
        }
    }

    #[test]
    fn float_uniform_equal_adjacent_and_extreme_bounds_follow_closed_open_contract() {
        let sampler = DeterministicSampler::new([3; 32]);
        let equal = Distribution::<f32>::try_uniform(-3.5, -3.5).expect("checked bounds");

        assert_eq!(
            sampler.f32(&equal, Field::MassKg, 1).to_bits(),
            (-3.5_f32).to_bits()
        );

        let min = 1.0_f32;
        let max = f32::from_bits(min.to_bits() + 1);
        assert_eq!(
            uniform_f32_from_bits(min, max, u64::MAX).to_bits(),
            min.to_bits()
        );

        let extreme = uniform_f32_from_bits(-f32::MAX, f32::MAX, u64::MAX);
        assert!(extreme.is_finite());
        assert!((-f32::MAX..f32::MAX).contains(&extreme));
    }

    #[test]
    fn float_uniform_clamps_a_round_to_maximum_result_to_the_next_lower_float() {
        #[allow(clippy::cast_possible_truncation)]
        let raw = (f64::from(1.0_f32)
            + (f64::from(2.0_f32) - f64::from(1.0_f32)) * (1.0 - 2_f64.powi(-53)))
            as f32;
        assert_eq!(raw.to_bits(), 2.0_f32.to_bits());

        assert_eq!(
            uniform_f32_from_bits(1.0, 2.0, u64::MAX).to_bits(),
            next_down(2.0).to_bits()
        );
    }
}
