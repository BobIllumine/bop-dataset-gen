//! Library mesh optimization: welding, bounded simplification, normals, GLB output.

use bop_assets::{
    GlbLoadLimits, LoadedVisualAsset, MeshOptimizationError, MeshOptimizationOptions, VisualMesh,
    VisualPrimitive, load_glb, optimize_visual_asset,
};

/// An unindexed (every triangle owns its corners) mesh with face normals, as
/// STL-derived files are.
fn unwelded(triangles: &[[[f32; 3]; 3]]) -> LoadedVisualAsset {
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    for [a, b, c] in triangles {
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        positions.extend([*a, *b, *c]);
        normals.extend([n; 3]);
    }
    let indices = (0..u32::try_from(positions.len()).unwrap()).collect();
    let primitive =
        VisualPrimitive::try_new(positions, normals, Vec::new(), indices, None).unwrap();
    LoadedVisualAsset::from_unmaterialized_mesh(VisualMesh::try_new(vec![primitive]).unwrap())
}

fn cube(half: f32) -> Vec<[[f32; 3]; 3]> {
    let c = |x: f32, y: f32, z: f32| [x * half, y * half, z * half];
    let quads = [
        [
            c(-1., -1., -1.),
            c(-1., 1., -1.),
            c(1., 1., -1.),
            c(1., -1., -1.),
        ],
        [
            c(-1., -1., 1.),
            c(1., -1., 1.),
            c(1., 1., 1.),
            c(-1., 1., 1.),
        ],
        [
            c(-1., -1., -1.),
            c(1., -1., -1.),
            c(1., -1., 1.),
            c(-1., -1., 1.),
        ],
        [
            c(-1., 1., -1.),
            c(-1., 1., 1.),
            c(1., 1., 1.),
            c(1., 1., -1.),
        ],
        [
            c(-1., -1., -1.),
            c(-1., -1., 1.),
            c(-1., 1., 1.),
            c(-1., 1., -1.),
        ],
        [
            c(1., -1., -1.),
            c(1., 1., -1.),
            c(1., 1., 1.),
            c(1., -1., 1.),
        ],
    ];
    quads
        .iter()
        .flat_map(|[a, b, c, d]| [[*a, *b, *c], [*a, *c, *d]])
        .collect()
}

/// A closed UV sphere of `radius` metres.
fn sphere(radius: f32, rings: u16, segments: u16) -> Vec<[[f32; 3]; 3]> {
    let point = |ring: u16, segment: u16| {
        let theta = std::f32::consts::PI * f32::from(ring) / f32::from(rings);
        let phi = std::f32::consts::TAU * f32::from(segment) / f32::from(segments);
        [
            radius * theta.sin() * phi.cos(),
            radius * theta.sin() * phi.sin(),
            radius * theta.cos(),
        ]
    };
    let mut triangles = Vec::new();
    for ring in 0..rings {
        for segment in 0..segments {
            let (a, b) = (point(ring, segment), point(ring, segment + 1));
            let (c, d) = (point(ring + 1, segment + 1), point(ring + 1, segment));
            if ring > 0 {
                triangles.push([a, d, b]);
            }
            if ring + 1 < rings {
                triangles.push([b, d, c]);
            }
        }
    }
    triangles
}

fn reload(glb: &[u8]) -> LoadedVisualAsset {
    let limits = GlbLoadLimits::try_new(16, 8, 1 << 20, 1 << 22, 1024, 1 << 20, 1 << 22).unwrap();
    load_glb(glb, limits).expect("the optimizer writes GLB our loader accepts")
}

#[test]
fn welding_alone_shares_corners_and_keeps_cube_edges_sharp() {
    let options = MeshOptimizationOptions::try_new(0.0, 30.0).unwrap();
    let optimized = optimize_visual_asset(unwelded(&cube(0.05)), options).unwrap();
    let report = optimized.report;
    assert_eq!((report.source_triangles, report.source_vertices), (12, 36));
    // 8 corners, each split three ways at the 90-degree creases.
    assert_eq!((report.triangles, report.vertices), (12, 24));
    assert_eq!(report.max_deviation_m.to_bits(), 0.0_f32.to_bits());

    let reloaded = reload(&optimized.glb);
    let primitive = &reloaded.visual_mesh().primitives()[0];
    assert_eq!(primitive.indices().len(), 36);
    for (position, normal) in primitive.positions().iter().zip(primitive.normals()) {
        // Every normal is one of the face axes, pointing out of the cube.
        let axis = normal
            .iter()
            .position(|c| c.abs() > 0.999)
            .expect("axis-aligned normal");
        assert!((position[axis] * normal[axis] - 0.05).abs() < 1e-6);
    }
}

#[test]
fn a_flat_grid_collapses_to_few_triangles_without_deviation() {
    let n: u16 = 20;
    let step = 0.1 / f32::from(n);
    let mut grid = Vec::new();
    for i in 0..n {
        for j in 0..n {
            let p = |a: u16, b: u16| [f32::from(a) * step, f32::from(b) * step, 0.0];
            grid.push([p(i, j), p(i + 1, j), p(i + 1, j + 1)]);
            grid.push([p(i, j), p(i + 1, j + 1), p(i, j + 1)]);
        }
    }
    let options = MeshOptimizationOptions::try_new(0.0001, 30.0).unwrap();
    let optimized = optimize_visual_asset(unwelded(&grid), options).unwrap();
    assert!(optimized.report.triangles < 100, "{:?}", optimized.report);
    assert!(optimized.report.max_deviation_m < 1e-6);
    let reloaded = reload(&optimized.glb);
    assert!(
        reloaded.visual_mesh().primitives()[0]
            .normals()
            .iter()
            .all(|n| n[2] > 0.999)
    );
}

#[test]
fn simplification_stays_within_the_budget_on_a_curved_surface() {
    let radius = 0.05;
    let budget = 0.0005;
    let options = MeshOptimizationOptions::try_new(budget, 60.0).unwrap();
    let optimized = optimize_visual_asset(unwelded(&sphere(radius, 64, 128)), options).unwrap();
    assert!(
        optimized.report.triangles < optimized.report.source_triangles / 4,
        "{:?}",
        optimized.report
    );
    assert!(optimized.report.max_deviation_m <= budget);

    // Independent check against the analytic sphere: vertices stay on it, and
    // no triangle centre or edge midpoint sags further inside than the budget.
    let reloaded = reload(&optimized.glb);
    let primitive = &reloaded.visual_mesh().primitives()[0];
    let p = primitive.positions();
    let length = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let mid = |a: [f32; 3], b: [f32; 3]| {
        [
            f32::midpoint(a[0], b[0]),
            f32::midpoint(a[1], b[1]),
            f32::midpoint(a[2], b[2]),
        ]
    };
    assert!(p.iter().all(|&v| (length(v) - radius).abs() < 1e-5));
    for t in primitive.indices().chunks_exact(3) {
        let [a, b, c] = [p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]];
        let centre = [
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ];
        for sample in [centre, mid(a, b), mid(b, c), mid(c, a)] {
            assert!(
                radius - length(sample) <= budget * 1.001,
                "sag {}",
                radius - length(sample)
            );
        }
    }
}

#[test]
fn invalid_options_are_rejected() {
    for (deviation, crease) in [
        (-0.001, 30.0),
        (f32::NAN, 30.0),
        (0.001, -1.0),
        (0.001, 181.0),
    ] {
        assert!(matches!(
            MeshOptimizationOptions::try_new(deviation, crease),
            Err(MeshOptimizationError::InvalidOptions(_))
        ));
    }
}
