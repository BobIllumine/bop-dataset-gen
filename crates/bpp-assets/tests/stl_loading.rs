//! Public integration coverage for bounded ASCII and binary STL ingestion.

use bpp_assets::{InvalidStlLoadOptionsError, StlLoadError, StlLoadOptions, load_stl};

fn binary_stl(triangles: &[[[f32; 3]; 4]]) -> Vec<u8> {
    let mut bytes = vec![0_u8; 80];
    bytes.extend_from_slice(
        &u32::try_from(triangles.len())
            .expect("test fixture triangle count fits u32")
            .to_le_bytes(),
    );
    for triangle in triangles {
        for vector in triangle {
            for component in vector {
                bytes.extend_from_slice(&component.to_le_bytes());
            }
        }
        bytes.extend_from_slice(&0_u16.to_le_bytes());
    }
    bytes
}

fn options(scale: f32, max_triangles: usize) -> StlLoadOptions {
    StlLoadOptions::try_new(scale, max_triangles).expect("test options are valid")
}

#[test]
fn ascii_stl_is_scaled_to_metres_and_uses_flat_generated_normals() {
    let source = br"solid plate
facet normal 1 0 0
outer loop
vertex 0 0 0
vertex 1000 0 0
vertex 0 1000 0
endloop
endfacet
endsolid plate
";

    let mesh = load_stl(source, options(0.001, 4)).expect("valid ASCII STL loads");
    let [primitive] = mesh.primitives() else {
        panic!("STL must produce exactly one primitive");
    };

    assert_eq!(
        primitive.positions(),
        &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]
    );
    assert_eq!(primitive.normals(), &[[0.0, 0.0, 1.0]; 3]);
    assert_eq!(primitive.indices(), &[0, 1, 2]);
    assert!(primitive.tex_coord_sets().is_empty());
    assert_eq!(primitive.material(), None);
}

#[test]
fn binary_stl_ignores_declared_normal_and_preserves_triangle_order() {
    let source = binary_stl(&[
        [
            [f32::NAN, f32::INFINITY, f32::NEG_INFINITY],
            [0.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.0, 0.0, 2.0],
        ],
        [
            [-99.0, -98.0, -97.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 2.0],
            [1.0, 2.0, 0.0],
        ],
    ]);

    let mesh = load_stl(&source, options(0.5, 2)).expect("valid binary STL loads");
    let primitive = &mesh.primitives()[0];

    assert_eq!(
        primitive.positions(),
        &[
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.5, 0.0, 0.0],
            [0.5, 0.0, 1.0],
            [0.5, 1.0, 0.0],
        ]
    );
    assert_eq!(
        primitive.normals(),
        &[
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
        ]
    );
    assert_eq!(primitive.indices(), &[0, 1, 2, 3, 4, 5]);
}

#[test]
fn options_require_positive_finite_units_and_nonzero_triangle_limit() {
    for invalid_scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert_eq!(
            StlLoadOptions::try_new(invalid_scale, 1),
            Err(InvalidStlLoadOptionsError::InvalidSourceUnitScale)
        );
    }
    assert_eq!(
        StlLoadOptions::try_new(1.0, 0),
        Err(InvalidStlLoadOptionsError::ZeroMaxTriangles)
    );
}

#[test]
fn triangle_limit_is_enforced_before_emitting_excess_geometry() {
    let triangle = [
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
    ];
    let source = binary_stl(&[triangle, triangle]);

    assert!(matches!(
        load_stl(&source, options(1.0, 1)),
        Err(StlLoadError::TriangleLimitExceeded { limit: 1 })
    ));
}

#[test]
fn malformed_triangle_after_limit_is_reported_as_malformed() {
    let mut source = binary_stl(&[[
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
    ]]);
    source[80..84].copy_from_slice(&2_u32.to_le_bytes());

    assert!(matches!(
        load_stl(&source, options(1.0, 1)),
        Err(StlLoadError::Malformed { .. })
    ));
}

#[test]
fn binary_stl_with_ascii_looking_header_is_still_detected_as_binary() {
    let mut source = binary_stl(&[[
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
    ]]);
    source[..21].copy_from_slice(b"solid deceptive name\n");

    let mesh = load_stl(&source, options(1.0, 1)).expect("valid binary STL loads");

    assert_eq!(mesh.primitives()[0].indices(), &[0, 1, 2]);
}

#[test]
fn malformed_stl_is_rejected() {
    assert!(matches!(
        load_stl(b"this is not STL", options(1.0, 1)),
        Err(StlLoadError::Malformed { .. })
    ));
}

#[test]
fn empty_stl_is_rejected() {
    let empty = binary_stl(&[]);
    assert!(matches!(
        load_stl(&empty, options(1.0, 1)),
        Err(StlLoadError::EmptyMesh)
    ));
}

#[test]
fn non_finite_source_coordinates_are_rejected() {
    let source = binary_stl(&[[
        [0.0, 0.0, 1.0],
        [f32::NAN, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
    ]]);

    assert!(matches!(
        load_stl(&source, options(1.0, 1)),
        Err(StlLoadError::NonFiniteCoordinate {
            triangle: 0,
            vertex: 0,
            component: 0,
        })
    ));
}

#[test]
fn coordinate_scaling_overflow_is_rejected() {
    let source = binary_stl(&[[
        [0.0, 0.0, 1.0],
        [f32::MAX, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
    ]]);

    assert!(matches!(
        load_stl(&source, options(2.0, 1)),
        Err(StlLoadError::ScaledCoordinateNotFinite {
            triangle: 0,
            vertex: 0,
            component: 0,
        })
    ));
}

#[test]
fn degenerate_triangles_are_rejected() {
    let source = binary_stl(&[[
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0],
        [2.0, 2.0, 2.0],
    ]]);

    assert!(matches!(
        load_stl(&source, options(1.0, 1)),
        Err(StlLoadError::DegenerateTriangle { triangle: 0 })
    ));
}

#[test]
fn large_finite_triangles_generate_representable_unit_normals() {
    let source = binary_stl(&[[
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [1.0e20, 0.0, 0.0],
        [0.0, 1.0e20, 0.0],
    ]]);

    let mesh = load_stl(&source, options(1.0, 1)).expect("finite triangle loads");

    assert_eq!(mesh.primitives()[0].normals(), &[[0.0, 0.0, 1.0]; 3]);
}

#[test]
fn small_finite_triangles_generate_representable_unit_normals() {
    let source = binary_stl(&[[
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [1.0e-20, 0.0, 0.0],
        [0.0, 1.0e-20, 0.0],
    ]]);

    let mesh = load_stl(&source, options(1.0, 1)).expect("finite triangle loads");

    assert_eq!(mesh.primitives()[0].normals(), &[[0.0, 0.0, 1.0]; 3]);
}

#[test]
fn loaded_geometry_canonicalizes_negative_zero() {
    let source = binary_stl(&[[
        [0.0, 0.0, 1.0],
        [-0.0, 0.0, 0.0],
        [1.0, -0.0, 0.0],
        [0.0, 1.0, -0.0],
    ]]);

    let mesh = load_stl(&source, options(1.0, 1)).expect("valid STL loads");
    let primitive = &mesh.primitives()[0];

    for vector in primitive.positions().iter().chain(primitive.normals()) {
        for component in vector {
            if *component == 0.0 {
                assert_eq!(component.to_bits(), 0.0_f32.to_bits());
            }
        }
    }
}
