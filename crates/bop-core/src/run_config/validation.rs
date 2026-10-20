//! Checked conversions from wire values to validated settings.

#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the parent's imports and helpers"
)]
use super::*;

pub(super) fn validate_assets(
    wires: Vec<AssetWire>,
) -> Result<Vec<AssetConfig>, RunConfigValidationError> {
    if wires.is_empty() {
        return Err(RunConfigValidationError::EmptyAssets);
    }
    let mut assets = Vec::with_capacity(wires.len());
    let mut ids = BTreeSet::new();
    for wire in wires {
        let asset = AssetConfig::try_from_wire(wire)?;
        if !ids.insert(asset.id.clone()) {
            return Err(RunConfigValidationError::DuplicateAssetId { asset_id: asset.id });
        }
        assets.push(asset);
    }
    if assets.iter().all(|asset| asset.mixture_weight == 0.0) {
        return Err(RunConfigValidationError::AllZeroAssetWeights);
    }
    assets.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(assets)
}

pub(super) fn validate_cameras(
    wires: Vec<CameraWire>,
) -> Result<Vec<CameraConfig>, RunConfigValidationError> {
    if wires.is_empty() {
        return Err(RunConfigValidationError::EmptyCameras);
    }
    let mut cameras = Vec::with_capacity(wires.len());
    let mut ids = BTreeSet::new();
    for wire in wires {
        let camera = CameraConfig::try_from_wire(wire)?;
        if !ids.insert(camera.id) {
            return Err(RunConfigValidationError::DuplicateCameraId {
                camera_id: camera.id,
            });
        }
        cameras.push(camera);
    }
    cameras.sort_by_key(|camera| camera.id);
    Ok(cameras)
}

pub(super) fn validate_output(
    wire: OutputWire,
    maximum: u32,
) -> Result<RenderChannels, RunConfigValidationError> {
    let channels = RenderChannels::try_new(wire.rgb, wire.depth, wire.visible_instances)
        .map_err(|_| RunConfigValidationError::NoOutputChannels)?;
    if wire.visible_instances && maximum > u32::from(u16::MAX) {
        return Err(RunConfigValidationError::ExcessiveInstanceCount { maximum });
    }
    Ok(channels)
}

pub(super) fn checked_object_count(
    wire: DistributionWire<u32>,
) -> Result<Distribution<u32>, RunConfigValidationError> {
    let invalid =
        |minimum, maximum| RunConfigValidationError::InvalidPopulationCount { minimum, maximum };
    match wire {
        DistributionWire::Fixed { value } => {
            if value == 0 {
                return Err(invalid(0, 0));
            }
            Ok(Distribution::<u32>::fixed(value))
        }
        DistributionWire::Uniform { min, max } => {
            if min == 0 || max == 0 || min > max {
                return Err(invalid(min, max));
            }
            Distribution::<u32>::try_uniform(min, max).map_err(|source| {
                invalid_distribution(DistributionField::ObjectCount, None, source.into())
            })
        }
        DistributionWire::Choice { values } => {
            let mut checked = Vec::with_capacity(values.len());
            let mut minimum = u32::MAX;
            let mut maximum = 0;
            for (index, value) in values.into_iter().enumerate() {
                if value.value == 0 {
                    return Err(invalid(0, maximum));
                }
                minimum = minimum.min(value.value);
                maximum = maximum.max(value.value);
                checked.push(WeightedValue::try_new(value.value, value.weight).map_err(
                    |source| {
                        invalid_distribution(
                            DistributionField::ObjectCount,
                            None,
                            ConfigDistributionError::Weight { index, source },
                        )
                    },
                )?);
            }
            Distribution::<u32>::try_choice(checked).map_err(|source| {
                invalid_distribution(DistributionField::ObjectCount, None, source.into())
            })
        }
    }
}

pub(super) fn checked_asset_distribution(
    wire: DistributionWire<String>,
) -> Result<Distribution<AssetId>, RunConfigValidationError> {
    match wire {
        DistributionWire::Fixed { value } => {
            Ok(Distribution::<AssetId>::fixed(asset_reference(value)?))
        }
        DistributionWire::Uniform { .. } => Err(invalid_distribution(
            DistributionField::AssetMixture,
            None,
            InvalidDistributionError::UniformAsset.into(),
        )),
        DistributionWire::Choice { values } => {
            let mut checked = Vec::with_capacity(values.len());
            for (index, value) in values.into_iter().enumerate() {
                checked.push(
                    WeightedValue::try_new(asset_reference(value.value)?, value.weight).map_err(
                        |source| {
                            invalid_distribution(
                                DistributionField::AssetMixture,
                                None,
                                ConfigDistributionError::Weight { index, source },
                            )
                        },
                    )?,
                );
            }
            Distribution::<AssetId>::try_choice(checked).map_err(|source| {
                invalid_distribution(DistributionField::AssetMixture, None, source.into())
            })
        }
    }
}

pub(super) fn checked_float_distribution(
    wire: DistributionWire<f32>,
    field: DistributionField,
    asset_id: Option<&AssetId>,
    domain: FloatDomain,
) -> Result<Distribution<f32>, RunConfigValidationError> {
    let distribution = match wire {
        DistributionWire::Fixed { value } => Distribution::<f32>::try_fixed(value)
            .map_err(|source| invalid_distribution(field, asset_id, source.into()))?,
        DistributionWire::Uniform { min, max } => Distribution::<f32>::try_uniform(min, max)
            .map_err(|source| invalid_distribution(field, asset_id, source.into()))?,
        DistributionWire::Choice { values } => {
            let mut checked = Vec::with_capacity(values.len());
            for (index, value) in values.into_iter().enumerate() {
                checked.push(WeightedValue::try_new(value.value, value.weight).map_err(
                    |source| {
                        invalid_distribution(
                            field,
                            asset_id,
                            ConfigDistributionError::Weight { index, source },
                        )
                    },
                )?);
            }
            Distribution::<f32>::try_choice(checked)
                .map_err(|source| invalid_distribution(field, asset_id, source.into()))?
        }
    };
    for value in distribution_values(&distribution) {
        if !domain.accepts(*value) {
            return Err(invalid_distribution(
                field,
                asset_id,
                ConfigDistributionError::DomainValue { value: *value },
            ));
        }
    }
    Ok(distribution)
}

pub(super) fn distribution_values<T>(distribution: &Distribution<T>) -> Vec<&T> {
    match distribution.view() {
        DistributionView::Fixed { value } => vec![value],
        DistributionView::Uniform { min, max, .. } => vec![min, max],
        DistributionView::Choice { values } => values.iter().map(WeightedValue::value).collect(),
    }
}

pub(super) fn maximum_count(distribution: &Distribution<u32>) -> u32 {
    match distribution.view() {
        DistributionView::Fixed { value } => *value,
        DistributionView::Uniform { max, .. } => *max,
        DistributionView::Choice { values } => values
            .iter()
            .filter(|entry| entry.weight() > 0.0)
            .map(|entry| *entry.value())
            .max()
            .expect("validated choice distributions have a positive weight"),
    }
}

#[derive(Clone, Copy)]
pub(super) enum FloatDomain {
    Positive,
    NonNegative,
    UnitInterval,
}

impl FloatDomain {
    pub(super) fn accepts(self, value: f32) -> bool {
        match self {
            Self::Positive => value > 0.0,
            Self::NonNegative => value >= 0.0,
            Self::UnitInterval => (0.0..=1.0).contains(&value),
        }
    }
}

pub(super) fn checked_projection(
    wire: CameraProjectionWire,
    camera_id: CameraId,
) -> Result<CameraProjection, RunConfigValidationError> {
    let configured = u8::from(wire.opencv_k.is_some()) + u8::from(wire.vertical_fov_deg.is_some());
    if configured != 1 {
        return Err(RunConfigValidationError::InvalidCameraProjectionSources {
            camera_id,
            configured,
        });
    }
    if let Some(k) = wire.opencv_k {
        return CameraIntrinsics::try_new(k)
            .map(CameraProjection::OpenCvK)
            .map_err(|_| RunConfigValidationError::InvalidCameraIntrinsics { camera_id });
    }
    let value = wire
        .vertical_fov_deg
        .expect("exactly one projection source");
    if !value.is_finite() || !(0.0..180.0).contains(&value) {
        return Err(RunConfigValidationError::InvalidVerticalFov { camera_id, value });
    }
    Ok(CameraProjection::VerticalFov(VerticalFovDegrees(value)))
}

pub(super) fn checked_pose(
    wire: CameraPoseWire,
    camera_id: CameraId,
) -> Result<CameraPose, RunConfigValidationError> {
    let configured =
        u8::from(wire.fixed_world_from_camera.is_some()) + u8::from(wire.shell.is_some());
    if configured != 1 {
        return Err(RunConfigValidationError::InvalidCameraPoseSources {
            camera_id,
            configured,
        });
    }
    if let Some(transform) = wire.fixed_world_from_camera {
        return checked_transform(
            transform,
            ConfigTransformField::CameraFixedPose,
            None,
            Some(camera_id),
        )
        .map(CameraPose::FixedWorldFromCamera);
    }
    ShellCameraPose::try_from_wire(wire.shell.expect("exactly one pose source"), camera_id)
        .map(CameraPose::Shell)
}

impl ShellCameraPose {
    pub(super) fn try_from_wire(
        wire: ShellCameraPoseWire,
        camera_id: CameraId,
    ) -> Result<Self, RunConfigValidationError> {
        let min_radius_m = PositiveMeters::try_new(wire.min_radius_m)
            .map_err(|_| invalid_shell(camera_id, CameraShellField::Radius))?;
        let max_radius_m = PositiveMeters::try_new(wire.max_radius_m)
            .map_err(|_| invalid_shell(camera_id, CameraShellField::Radius))?;
        if min_radius_m.get() > max_radius_m.get() {
            return Err(invalid_shell(camera_id, CameraShellField::Radius));
        }
        if !wire.min_azimuth_deg.is_finite()
            || !wire.max_azimuth_deg.is_finite()
            || wire.min_azimuth_deg > wire.max_azimuth_deg
        {
            return Err(invalid_shell(camera_id, CameraShellField::Azimuth));
        }
        if !wire.min_elevation_deg.is_finite()
            || !wire.max_elevation_deg.is_finite()
            || wire.min_elevation_deg > wire.max_elevation_deg
        {
            return Err(invalid_shell(camera_id, CameraShellField::Elevation));
        }
        let target_bin_m = wire.target_bin_m.map(Meters::new);
        let [Some(x), Some(y), Some(z)] = target_bin_m else {
            return Err(invalid_shell(camera_id, CameraShellField::Target));
        };
        Ok(Self {
            min_radius_m,
            max_radius_m,
            min_azimuth_deg: canonicalize_zero(wire.min_azimuth_deg),
            max_azimuth_deg: canonicalize_zero(wire.max_azimuth_deg),
            min_elevation_deg: canonicalize_zero(wire.min_elevation_deg),
            max_elevation_deg: canonicalize_zero(wire.max_elevation_deg),
            target_bin_m: [x, y, z],
        })
    }
}

pub(super) fn checked_image_size(
    wire: ImageSizeWire,
    camera_id: CameraId,
) -> Result<ImageSize, RunConfigValidationError> {
    ImageSize::try_new(wire.width, wire.height)
        .map_err(|_| RunConfigValidationError::InvalidCameraImageSize { camera_id })
}

pub(super) fn checked_ambient(wire: AmbientWire) -> Result<AmbientLight, RunConfigValidationError> {
    let color = LinearRgb::try_new(wire.color)
        .map_err(|_| invalid_appearance(AppearanceField::Ambient, None))?;
    let intensity = NonNegativeFinite::try_new(wire.intensity)
        .map_err(|_| invalid_appearance(AppearanceField::Ambient, None))?;
    Ok(AmbientLight::new(color, intensity))
}

pub(super) fn checked_directional_light(
    wire: DirectionalLightWire,
    index: usize,
) -> Result<DirectionalLight, RunConfigValidationError> {
    let color = LinearRgb::try_new(wire.color)
        .map_err(|_| invalid_appearance(AppearanceField::DirectionalLight, Some(index)))?;
    let illuminance = NonNegativeFinite::try_new(wire.illuminance_lux)
        .map_err(|_| invalid_appearance(AppearanceField::DirectionalLight, Some(index)))?;
    DirectionalLight::try_new(wire.direction_world, color, illuminance)
        .map_err(|_| invalid_appearance(AppearanceField::DirectionalLight, Some(index)))
}

pub(super) fn checked_point_light(
    wire: PointLightWire,
    index: usize,
) -> Result<PointLight, RunConfigValidationError> {
    let color = LinearRgb::try_new(wire.color)
        .map_err(|_| invalid_appearance(AppearanceField::PointLight, Some(index)))?;
    let power = NonNegativeFinite::try_new(wire.luminous_power_lm)
        .map_err(|_| invalid_appearance(AppearanceField::PointLight, Some(index)))?;
    let position = wire.position_world_m.map(Meters::new);
    let [Some(x), Some(y), Some(z)] = position else {
        return Err(invalid_appearance(AppearanceField::PointLight, Some(index)));
    };
    Ok(PointLight::new([x, y, z], color, power))
}

pub(super) fn invalid_appearance(
    field: AppearanceField,
    index: Option<usize>,
) -> RunConfigValidationError {
    RunConfigValidationError::InvalidAppearance { field, index }
}

pub(super) fn invalid_shell(
    camera_id: CameraId,
    field: CameraShellField,
) -> RunConfigValidationError {
    RunConfigValidationError::InvalidCameraShell { camera_id, field }
}

pub(super) fn invalid_distribution(
    field: DistributionField,
    asset_id: Option<&AssetId>,
    source: ConfigDistributionError,
) -> RunConfigValidationError {
    RunConfigValidationError::InvalidDistribution {
        field,
        asset_id: asset_id.cloned(),
        source,
    }
}

pub(super) fn asset_reference(value: String) -> Result<AssetId, RunConfigValidationError> {
    AssetId::new(value.clone()).ok_or(RunConfigValidationError::InvalidAssetReference { value })
}

pub(super) fn checked_transform(
    wire: TransformWire,
    field: ConfigTransformField,
    asset_id: Option<&AssetId>,
    camera_id: Option<CameraId>,
) -> Result<RigidTransform, RunConfigValidationError> {
    RigidTransform::try_new(wire.translation, wire.rotation_xyzw).ok_or_else(|| {
        RunConfigValidationError::InvalidTransform {
            field,
            asset_id: asset_id.cloned(),
            camera_id,
        }
    })
}

pub(super) fn checked_path(
    value: String,
    field: ConfigPathField,
    asset_id: Option<&AssetId>,
) -> Result<PathBuf, RunConfigValidationError> {
    if value.trim().is_empty() {
        return Err(RunConfigValidationError::InvalidPath {
            field,
            asset_id: asset_id.cloned(),
        });
    }
    Ok(PathBuf::from(value))
}

pub(super) fn nonzero(value: u32, field: ConfigCountField) -> Result<(), RunConfigValidationError> {
    if value == 0 {
        Err(RunConfigValidationError::ZeroCount { field })
    } else {
        Ok(())
    }
}

pub(super) fn positive_meters(
    value: f32,
    field: ConfigScalarField,
) -> Result<PositiveMeters, RunConfigValidationError> {
    PositiveMeters::try_new(value).map_err(|_| RunConfigValidationError::InvalidPositiveScalar {
        field,
        value: f64::from(value),
    })
}

pub(super) fn positive_f32(
    value: f32,
    field: ConfigScalarField,
) -> Result<(), RunConfigValidationError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(RunConfigValidationError::InvalidPositiveScalar {
            field,
            value: f64::from(value),
        })
    }
}

pub(super) fn nonnegative_f32(
    value: f32,
    field: ConfigScalarField,
) -> Result<(), RunConfigValidationError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(RunConfigValidationError::InvalidNonNegativeScalar {
            field,
            value: f64::from(value),
        })
    }
}

pub(super) fn unit_interval(
    value: f32,
    field: ConfigScalarField,
) -> Result<(), RunConfigValidationError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(RunConfigValidationError::InvalidUnitInterval {
            field,
            value: f64::from(value),
        })
    }
}

pub(super) const fn axis(index: usize) -> Axis {
    match index {
        0 => Axis::X,
        1 => Axis::Y,
        _ => Axis::Z,
    }
}

pub(super) const fn canonicalize_zero(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

pub(super) const fn canonicalize_zero_f64(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}
