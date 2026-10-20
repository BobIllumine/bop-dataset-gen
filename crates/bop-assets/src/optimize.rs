//! Mesh optimization for the asset library: weld, simplify within a measured
//! error budget, rebuild normals, and write the result as GLB.

use std::collections::HashMap;

use meshopt::{SimplifyOptions, VertexDataAdapter};
use parry3d::{math::Vec3, query::PointQuery, shape::TriMesh};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{LoadedVisualAsset, PbrMaterial, VisualPrimitive};

/// Simplification passes tried, each with half the previous error target,
/// before the measured deviation must be within the budget.
const MAX_SIMPLIFY_PASSES: usize = 8;

/// Checked optimization settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshOptimizationOptions {
    max_deviation_m: f32,
    crease_angle_deg: f32,
}

impl MeshOptimizationOptions {
    /// Creates options. A zero deviation only welds; the crease angle splits
    /// normals between faces that meet at a sharper angle.
    ///
    /// # Errors
    ///
    /// Returns [`MeshOptimizationError::InvalidOptions`] for a negative or
    /// non-finite deviation, or a crease angle outside 0 to 180 degrees.
    pub fn try_new(
        max_deviation_m: f32,
        crease_angle_deg: f32,
    ) -> Result<Self, MeshOptimizationError> {
        if !max_deviation_m.is_finite() || max_deviation_m < 0.0 {
            return Err(MeshOptimizationError::InvalidOptions(
                "max_deviation_m must be finite and nonnegative",
            ));
        }
        if !crease_angle_deg.is_finite() || !(0.0..=180.0).contains(&crease_angle_deg) {
            return Err(MeshOptimizationError::InvalidOptions(
                "crease_angle_deg must be between 0 and 180",
            ));
        }
        Ok(Self {
            max_deviation_m,
            crease_angle_deg,
        })
    }

    /// Returns the maximum allowed surface deviation in metres.
    #[must_use]
    pub const fn max_deviation_m(self) -> f32 {
        self.max_deviation_m
    }

    /// Returns the normal-splitting crease angle in degrees.
    #[must_use]
    pub const fn crease_angle_deg(self) -> f32 {
        self.crease_angle_deg
    }
}

/// What an optimization changed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshOptimizationReport {
    /// Source triangles over all primitives.
    pub source_triangles: usize,
    /// Source vertices over all primitives.
    pub source_vertices: usize,
    /// Optimized triangles.
    pub triangles: usize,
    /// Optimized vertices, including those split at creases.
    pub vertices: usize,
    /// Largest measured distance, in metres, from a source vertex or triangle
    /// centre to the optimized surface.
    pub max_deviation_m: f32,
    /// Texture-coordinate sets removed; no texture referenced them.
    pub dropped_tex_coord_sets: usize,
}

/// An optimized asset, encoded as a binary glTF in metres.
#[derive(Clone, Debug, PartialEq)]
pub struct OptimizedAsset {
    /// GLB file bytes.
    pub glb: Vec<u8>,
    /// What changed.
    pub report: MeshOptimizationReport,
}

/// Why an asset cannot be optimized.
#[derive(Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum MeshOptimizationError {
    /// Options are out of range.
    #[error("invalid optimization options: {0}")]
    InvalidOptions(&'static str),
    /// Textured assets would need their UV seams preserved, which is not supported.
    #[error("textured assets cannot be optimized")]
    Textured,
    /// Welding left no triangle with three distinct corners.
    #[error("the mesh has no non-degenerate triangles")]
    Degenerate,
    /// The simplifier rejected its input.
    #[error("simplification failed: {0}")]
    Simplify(String),
    /// The deviation could not be measured.
    #[error("cannot measure deviation: {0}")]
    Measure(String),
    /// Even the most conservative pass exceeded the budget.
    #[error("no simplification within {budget_m} m found; the closest deviates {measured_m} m")]
    BudgetUnreachable {
        /// Requested budget.
        budget_m: f32,
        /// Smallest measured deviation.
        measured_m: f32,
    },
}

/// One welded primitive: unique positions, triangles, and its material.
struct Welded {
    positions: Vec<[f32; 3]>,
    triangles: Vec<[u32; 3]>,
    material: Option<u32>,
}

/// Welds, simplifies within `options`' budget, rebuilds normals, and writes GLB.
///
/// # Errors
///
/// Returns [`MeshOptimizationError`] for textured or degenerate input, a
/// simplifier failure, or a budget that cannot be met.
pub fn optimize_visual_asset(
    asset: LoadedVisualAsset,
    options: MeshOptimizationOptions,
) -> Result<OptimizedAsset, MeshOptimizationError> {
    let (mesh, materials, textures) = asset.into_parts();
    if !textures.is_empty() || materials.iter().any(|m| m.base_color_texture().is_some()) {
        return Err(MeshOptimizationError::Textured);
    }
    let primitives = mesh.primitives();
    let source_triangles = primitives.iter().map(|p| p.indices().len() / 3).sum();
    let source_vertices = primitives.iter().map(|p| p.positions().len()).sum();
    let dropped_tex_coord_sets = primitives
        .iter()
        .map(|p| p.tex_coord_sets().len())
        .max()
        .unwrap_or(0);

    let welded = primitives.iter().map(weld).collect::<Result<Vec<_>, _>>()?;
    let source = Surface::try_new(&welded)?;
    let mut target = options.max_deviation_m;
    let mut closest = f32::INFINITY;
    let mut simplified = None;
    if target == 0.0 {
        simplified = Some(
            welded
                .iter()
                .map(|w| w.triangles.clone())
                .collect::<Vec<_>>(),
        );
        closest = 0.0;
    } else {
        for _ in 0..MAX_SIMPLIFY_PASSES {
            let candidate = welded
                .iter()
                .map(|w| simplify(w, target))
                .collect::<Result<Vec<_>, _>>()?;
            let deviation = source.deviation_to(&welded, &candidate)?;
            if deviation <= options.max_deviation_m {
                simplified = Some(candidate);
                closest = deviation;
                break;
            }
            closest = closest.min(deviation);
            target /= 2.0;
        }
    }
    let Some(simplified) = simplified else {
        return Err(MeshOptimizationError::BudgetUnreachable {
            budget_m: options.max_deviation_m,
            measured_m: closest,
        });
    };

    let cos_crease = options.crease_angle_deg.to_radians().cos();
    let outputs: Vec<OutputPrimitive> = welded
        .iter()
        .zip(&simplified)
        .map(|(w, triangles)| with_normals(&w.positions, triangles, cos_crease, w.material))
        .collect();
    let report = MeshOptimizationReport {
        source_triangles,
        source_vertices,
        triangles: outputs.iter().map(|p| p.indices.len() / 3).sum(),
        vertices: outputs.iter().map(|p| p.positions.len()).sum(),
        max_deviation_m: closest,
        dropped_tex_coord_sets,
    };
    Ok(OptimizedAsset {
        glb: write_glb(&outputs, &materials),
        report,
    })
}

/// Merges bit-identical positions and drops triangles that collapse.
fn weld(primitive: &VisualPrimitive) -> Result<Welded, MeshOptimizationError> {
    let mut ids: HashMap<[u32; 3], u32> = HashMap::new();
    let mut positions = Vec::new();
    let remap: Vec<u32> = primitive
        .positions()
        .iter()
        .map(|p| {
            // -0.0 and 0.0 are the same point.
            let key = p.map(|c| (c + 0.0).to_bits());
            *ids.entry(key).or_insert_with(|| {
                positions.push(*p);
                u32::try_from(positions.len() - 1).expect("vertex count fits u32")
            })
        })
        .collect();
    let triangles: Vec<[u32; 3]> = primitive
        .indices()
        .chunks_exact(3)
        .map(|t| {
            [
                remap[t[0] as usize],
                remap[t[1] as usize],
                remap[t[2] as usize],
            ]
        })
        .filter(|[a, b, c]| a != b && b != c && a != c)
        .collect();
    if triangles.is_empty() {
        return Err(MeshOptimizationError::Degenerate);
    }
    Ok(Welded {
        positions,
        triangles,
        material: primitive.material().map(crate::MaterialIndex::get),
    })
}

fn simplify(welded: &Welded, target_error_m: f32) -> Result<Vec<[u32; 3]>, MeshOptimizationError> {
    let indices: Vec<u32> = welded.triangles.iter().flatten().copied().collect();
    let bytes: Vec<u8> = welded
        .positions
        .iter()
        .flatten()
        .flat_map(|c| c.to_ne_bytes())
        .collect();
    let adapter = VertexDataAdapter::new(&bytes, 12, 0)
        .map_err(|e| MeshOptimizationError::Simplify(e.to_string()))?;
    let simplified = meshopt::simplify(
        &indices,
        &adapter,
        0,
        target_error_m,
        SimplifyOptions::ErrorAbsolute,
        None,
    );
    Ok(simplified
        .chunks_exact(3)
        .map(|t| [t[0], t[1], t[2]])
        .collect())
}

/// The welded source surface, for measuring how far a candidate strays from it.
struct Surface {
    mesh: TriMesh,
    /// Every source vertex and triangle centre.
    samples: Vec<Vec3>,
}

impl Surface {
    fn try_new(welded: &[Welded]) -> Result<Self, MeshOptimizationError> {
        let all: Vec<Vec<[u32; 3]>> = welded.iter().map(|w| w.triangles.clone()).collect();
        let (vertices, triangles) = combined(welded, &all);
        let samples = vertices
            .iter()
            .copied()
            .chain(triangles.iter().map(|t| centre(&vertices, t)))
            .collect();
        let mesh = TriMesh::new(vertices, triangles)
            .map_err(|e| MeshOptimizationError::Measure(e.to_string()))?;
        Ok(Self { mesh, samples })
    }

    /// Hausdorff-style deviation: the largest distance from a source sample
    /// to the candidate surface, or from a candidate triangle's centre or edge
    /// midpoint to the source surface.
    fn deviation_to(
        &self,
        welded: &[Welded],
        candidate: &[Vec<[u32; 3]>],
    ) -> Result<f32, MeshOptimizationError> {
        let (vertices, triangles) = combined(welded, candidate);
        if triangles.is_empty() {
            return Ok(f32::INFINITY);
        }
        let outward: Vec<Vec3> = triangles
            .iter()
            .flat_map(|t| {
                let [a, b, c] = t.map(|i| vertices[i as usize]);
                [
                    centre(&vertices, t),
                    (a + b) / 2.0,
                    (b + c) / 2.0,
                    (c + a) / 2.0,
                ]
            })
            .collect();
        let mesh = TriMesh::new(vertices, triangles)
            .map_err(|e| MeshOptimizationError::Measure(e.to_string()))?;
        let distance =
            |surface: &TriMesh, p: Vec3| (surface.project_local_point(p, false).point - p).length();
        let inward = self.samples.iter().map(|&p| distance(&mesh, p));
        let outward = outward.into_iter().map(|p| distance(&self.mesh, p));
        Ok(inward.chain(outward).fold(0.0, f32::max))
    }
}

/// All primitives' vertices and triangles as one indexed mesh.
fn combined(welded: &[Welded], triangles: &[Vec<[u32; 3]>]) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (w, tris) in welded.iter().zip(triangles) {
        let base = u32::try_from(vertices.len()).expect("vertex count fits u32");
        vertices.extend(w.positions.iter().map(|p| Vec3::from_array(*p)));
        indices.extend(tris.iter().map(|t| t.map(|i| i + base)));
    }
    (vertices, indices)
}

fn centre(vertices: &[Vec3], triangle: &[u32; 3]) -> Vec3 {
    triangle.iter().map(|&i| vertices[i as usize]).sum::<Vec3>() / 3.0
}

struct OutputPrimitive {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
    material: Option<u32>,
}

/// Gives each triangle corner the area-weighted normal of the faces around
/// its vertex that lie within the crease angle, then shares equal corners.
fn with_normals(
    positions: &[[f32; 3]],
    triangles: &[[u32; 3]],
    cos_crease: f32,
    material: Option<u32>,
) -> OutputPrimitive {
    let point = |i: u32| Vec3::from_array(positions[i as usize]);
    // Unnormalized: length is twice the area, so sums are area-weighted.
    let face: Vec<Vec3> = triangles
        .iter()
        .map(|t| (point(t[1]) - point(t[0])).cross(point(t[2]) - point(t[0])))
        .collect();
    let mut incident: Vec<Vec<usize>> = vec![Vec::new(); positions.len()];
    for (f, t) in triangles.iter().enumerate() {
        for &v in t {
            incident[v as usize].push(f);
        }
    }
    let mut output = OutputPrimitive {
        positions: Vec::new(),
        normals: Vec::new(),
        indices: Vec::with_capacity(triangles.len() * 3),
        material,
    };
    let mut corners: HashMap<(u32, [u32; 3]), u32> = HashMap::new();
    for (f, t) in triangles.iter().enumerate() {
        let own = face[f].normalize_or_zero();
        for &v in t {
            let normal = incident[v as usize]
                .iter()
                .filter(|&&g| face[g].normalize_or_zero().dot(own) >= cos_crease)
                .map(|&g| face[g])
                .sum::<Vec3>()
                .try_normalize()
                .unwrap_or(own)
                .to_array();
            let key = (v, normal.map(f32::to_bits));
            let index = *corners.entry(key).or_insert_with(|| {
                output.positions.push(positions[v as usize]);
                output.normals.push(normal);
                u32::try_from(output.positions.len() - 1).expect("vertex count fits u32")
            });
            output.indices.push(index);
        }
    }
    output.indices = meshopt::optimize_vertex_cache(&output.indices, output.positions.len());
    output
}

/// Encodes primitives and their materials as a single-mesh GLB.
fn write_glb(primitives: &[OutputPrimitive], materials: &[PbrMaterial]) -> Vec<u8> {
    let mut bin: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut push = |bytes: Vec<u8>, target: u32, accessor: Value| {
        let offset = bin.len();
        bin.extend(bytes);
        bin.resize(bin.len().next_multiple_of(4), 0);
        views.push(json!({"buffer": 0, "byteOffset": offset, "byteLength": bin.len() - offset, "target": target}));
        let mut accessor = accessor;
        accessor["bufferView"] = json!(views.len() - 1);
        accessors.push(accessor);
        accessors.len() - 1
    };
    let mut mesh_primitives = Vec::new();
    for p in primitives {
        let (min, max) = p
            .positions
            .iter()
            .fold(([f32::MAX; 3], [f32::MIN; 3]), |(lo, hi), q| {
                (
                    [lo[0].min(q[0]), lo[1].min(q[1]), lo[2].min(q[2])],
                    [hi[0].max(q[0]), hi[1].max(q[1]), hi[2].max(q[2])],
                )
            });
        let floats = |v: &[[f32; 3]]| {
            v.iter()
                .flatten()
                .flat_map(|c| c.to_le_bytes())
                .collect::<Vec<u8>>()
        };
        let position = push(
            floats(&p.positions),
            34962,
            json!({"componentType": 5126, "count": p.positions.len(), "type": "VEC3", "min": min, "max": max}),
        );
        let normal = push(
            floats(&p.normals),
            34962,
            json!({"componentType": 5126, "count": p.normals.len(), "type": "VEC3"}),
        );
        let indices = push(
            p.indices.iter().flat_map(|i| i.to_le_bytes()).collect(),
            34963,
            json!({"componentType": 5125, "count": p.indices.len(), "type": "SCALAR"}),
        );
        let mut primitive = json!({"attributes": {"POSITION": position, "NORMAL": normal}, "indices": indices, "mode": 4});
        if let Some(material) = p.material {
            primitive["material"] = json!(material);
        }
        mesh_primitives.push(primitive);
    }
    let mut document = json!({
        "asset": {"version": "2.0", "generator": "bop-assets mesh optimizer"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": mesh_primitives}],
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{"byteLength": bin.len()}],
    });
    if !materials.is_empty() {
        document["materials"] = materials
            .iter()
            .map(|m| {
                json!({"pbrMetallicRoughness": {
                    "baseColorFactor": m.base_color_factor(),
                    "metallicFactor": m.metallic_factor(),
                    "roughnessFactor": m.roughness_factor(),
                }})
            })
            .collect();
    }
    let mut text = serde_json::to_vec(&document).expect("glTF JSON serializes");
    text.resize(text.len().next_multiple_of(4), b' ');
    let total = 12 + 8 + text.len() + 8 + bin.len();
    let mut glb = Vec::with_capacity(total);
    glb.extend(b"glTF");
    glb.extend(2_u32.to_le_bytes());
    glb.extend(u32::try_from(total).expect("GLB fits u32").to_le_bytes());
    glb.extend(
        u32::try_from(text.len())
            .expect("JSON fits u32")
            .to_le_bytes(),
    );
    glb.extend(b"JSON");
    glb.extend(text);
    glb.extend(
        u32::try_from(bin.len())
            .expect("BIN fits u32")
            .to_le_bytes(),
    );
    glb.extend(b"BIN\0");
    glb.extend(bin);
    glb
}
