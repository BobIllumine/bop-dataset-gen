use std::f64::consts::PI;

use blake3::Hasher;
use bpp_core::{InstanceId, RigidTransform};

#[derive(Clone, Copy)]
pub(super) enum PlacementField {
    OrientationU1,
    OrientationU2,
    OrientationU3,
    CenterX,
    CenterY,
    CenterZ,
}

impl PlacementField {
    const fn label(self) -> &'static [u8] {
        match self {
            Self::OrientationU1 => b"bpp.dataset.placement-sampling.v1/orientation-u1",
            Self::OrientationU2 => b"bpp.dataset.placement-sampling.v1/orientation-u2",
            Self::OrientationU3 => b"bpp.dataset.placement-sampling.v1/orientation-u3",
            Self::CenterX => b"bpp.dataset.placement-sampling.v1/center-x",
            Self::CenterY => b"bpp.dataset.placement-sampling.v1/center-y",
            Self::CenterZ => b"bpp.dataset.placement-sampling.v1/center-z",
        }
    }
}

/// Stateless keyed addresses for placement candidates.
pub(super) struct PlacementSampler {
    key: [u8; 32],
}

impl PlacementSampler {
    pub(super) const fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub(super) fn unit(&self, field: PlacementField, instance: InstanceId, attempt: u32) -> f64 {
        unit_interval(self.bits(field, instance, attempt))
    }

    pub(super) fn orientation(&self, instance: InstanceId, attempt: u32) -> Option<RigidTransform> {
        let u1 = self.unit(PlacementField::OrientationU1, instance, attempt);
        let u2 = self.unit(PlacementField::OrientationU2, instance, attempt);
        let u3 = self.unit(PlacementField::OrientationU3, instance, attempt);
        let two_pi = 2.0 * PI;
        let xy = (1.0 - u1).sqrt();
        let zw = u1.sqrt();
        #[allow(clippy::cast_possible_truncation)]
        let rotation = [
            (xy * (two_pi * u2).sin()) as f32,
            (xy * (two_pi * u2).cos()) as f32,
            (zw * (two_pi * u3).sin()) as f32,
            (zw * (two_pi * u3).cos()) as f32,
        ];
        RigidTransform::try_new([0.0, 0.0, 0.0], rotation)
    }

    fn bits(&self, field: PlacementField, instance: InstanceId, attempt: u32) -> u64 {
        let mut hasher = Hasher::new_keyed(&self.key);
        hasher.update(field.label());
        hasher.update(&u32::from(instance.get()).to_le_bytes());
        hasher.update(&attempt.to_le_bytes());
        let digest = hasher.finalize();
        u64::from_le_bytes(
            digest.as_bytes()[..8]
                .try_into()
                .expect("BLAKE3 digests contain eight leading bytes"),
        )
    }
}

fn unit_interval(bits: u64) -> f64 {
    const SCALE: f64 = 1.0 / 9_007_199_254_740_992.0;
    #[allow(clippy::cast_precision_loss)]
    {
        (bits >> 11) as f64 * SCALE
    }
}

#[cfg(test)]
mod tests {
    use bpp_core::InstanceId;

    use super::{PlacementField, PlacementSampler};

    #[test]
    fn placement_addresses_are_independent_and_orientation_is_a_nontrivial_unit_quaternion() {
        let sampler = PlacementSampler::new([0x5a; 32]);
        let instance = InstanceId::new(7).expect("nonzero test ID");
        let fields = [
            PlacementField::OrientationU1,
            PlacementField::OrientationU2,
            PlacementField::OrientationU3,
            PlacementField::CenterX,
            PlacementField::CenterY,
            PlacementField::CenterZ,
        ];
        let addresses = fields
            .map(|field| sampler.bits(field, instance, 3))
            .to_vec();
        assert_eq!(
            addresses,
            vec![
                12_965_194_128_710_807_285,
                17_326_254_531_022_963_204,
                18_228_246_354_013_231_824,
                1_629_596_765_336_150_096,
                8_436_736_367_238_168_704,
                18_442_873_967_453_703_624,
            ]
        );
        assert_eq!(addresses.len(), 6);
        assert_eq!(
            addresses
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            6
        );
        assert_ne!(
            sampler.bits(PlacementField::CenterX, instance, 3),
            sampler.bits(PlacementField::CenterX, instance, 4)
        );
        assert_ne!(
            sampler.bits(PlacementField::CenterX, instance, 3),
            sampler.bits(
                PlacementField::CenterX,
                InstanceId::new(8).expect("nonzero test ID"),
                3
            )
        );

        let orientation = sampler
            .orientation(instance, 3)
            .expect("finite unit orientation");
        assert_eq!(
            orientation.rotation_xyzw().map(f32::to_bits),
            [3_192_907_695, 1_057_063_569, 3_179_238_286, 1_062_602_383]
        );
        let [x, y, z, w] = orientation.rotation_xyzw();
        assert!(x != 0.0 && y != 0.0 && z != 0.0);
        assert!((x.mul_add(x, y.mul_add(y, z.mul_add(z, w * w))) - 1.0).abs() < 1.0e-6);
    }
}
