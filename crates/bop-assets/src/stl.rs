use std::io::{Cursor, Read, Seek, SeekFrom};

use crate::{InvalidStlLoadOptionsError, StlLoadError, VisualMesh, VisualPrimitive};

/// Explicit unit conversion and resource bounds for STL ingestion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StlLoadOptions {
    source_unit_scale_m: f32,
    max_triangles: usize,
}

impl StlLoadOptions {
    /// Creates checked options with metres per source unit and a triangle limit.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidStlLoadOptionsError`] when the scale is not positive and
    /// finite or when `max_triangles` is zero.
    pub fn try_new(
        source_unit_scale_m: f32,
        max_triangles: usize,
    ) -> Result<Self, InvalidStlLoadOptionsError> {
        if !source_unit_scale_m.is_finite() || source_unit_scale_m <= 0.0 {
            return Err(InvalidStlLoadOptionsError::InvalidSourceUnitScale);
        }
        if max_triangles == 0 {
            return Err(InvalidStlLoadOptionsError::ZeroMaxTriangles);
        }
        Ok(Self {
            source_unit_scale_m,
            max_triangles,
        })
    }

    /// Returns metres represented by one coordinate unit in the source STL.
    #[must_use]
    pub const fn source_unit_scale_m(self) -> f32 {
        self.source_unit_scale_m
    }

    /// Returns the maximum number of triangles accepted from one STL.
    #[must_use]
    pub const fn max_triangles(self) -> usize {
        self.max_triangles
    }
}

/// Loads an ASCII or binary STL into one flat-shaded primitive in its source frame.
///
/// STL has no standard unit or axis metadata. [`StlLoadOptions`] supplies the
/// scale to metres; a later normalization stage must supply any source-frame to
/// canonical-frame rotation.
///
/// STL does not declare units, materials, texture coordinates, or reliable
/// normals. `options` therefore supplies mandatory metres-per-source-unit and
/// this loader derives one winding-based normal for each triangle. Triangle
/// corners are intentionally not welded so adjacent faces retain flat normals.
///
/// # Errors
///
/// Returns [`StlLoadError`] for malformed or empty input, resource-limit
/// violations, non-finite or overflowing coordinates, degenerate triangles,
/// or normalized-mesh invariant violations.
pub fn load_stl(bytes: &[u8], options: StlLoadOptions) -> Result<VisualMesh, StlLoadError> {
    if bytes.starts_with(b"solid ") && has_exact_binary_layout(bytes) {
        let mut cursor = BinaryProbeCursor::new(bytes);
        load_stl_from_reader(&mut cursor, options)
    } else {
        let mut cursor = Cursor::new(bytes);
        load_stl_from_reader(&mut cursor, options)
    }
}

fn load_stl_from_reader<R>(
    reader: &mut R,
    options: StlLoadOptions,
) -> Result<VisualMesh, StlLoadError>
where
    R: Read + Seek,
{
    let triangles = stl_io::create_stl_reader(reader).map_err(|error| StlLoadError::Malformed {
        message: error.to_string(),
    })?;

    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut indices = Vec::new();

    for (triangle_index, triangle) in triangles.enumerate() {
        let triangle = triangle.map_err(|error| StlLoadError::Malformed {
            message: error.to_string(),
        })?;
        if triangle_index == options.max_triangles {
            return Err(StlLoadError::TriangleLimitExceeded {
                limit: options.max_triangles,
            });
        }
        let vertices = scale_vertices(
            triangle.vertices,
            options.source_unit_scale_m,
            triangle_index,
        )?;
        let normal = generated_normal(vertices, triangle_index)?;

        let first_index =
            u32::try_from(positions.len()).map_err(|_| StlLoadError::VertexCountOverflow)?;
        if first_index > u32::MAX - 2 {
            return Err(StlLoadError::VertexCountOverflow);
        }
        positions.extend(vertices);
        normals.extend([normal; 3]);
        indices.extend([first_index, first_index + 1, first_index + 2]);
    }

    if positions.is_empty() {
        return Err(StlLoadError::EmptyMesh);
    }

    let primitive = VisualPrimitive::try_new(positions, normals, Vec::new(), indices, None)?;
    Ok(VisualMesh::try_new(vec![primitive])?)
}

fn has_exact_binary_layout(bytes: &[u8]) -> bool {
    const HEADER_AND_COUNT_BYTES: usize = 84;
    const TRIANGLE_BYTES: usize = 50;

    let Some(payload_bytes) = bytes.len().checked_sub(HEADER_AND_COUNT_BYTES) else {
        return false;
    };
    if !payload_bytes.is_multiple_of(TRIANGLE_BYTES) {
        return false;
    }
    let Ok(count_bytes) = <[u8; 4]>::try_from(&bytes[80..84]) else {
        return false;
    };
    usize::try_from(u32::from_le_bytes(count_bytes))
        .is_ok_and(|count| count == payload_bytes / TRIANGLE_BYTES)
}

struct BinaryProbeCursor<'a> {
    inner: Cursor<&'a [u8]>,
}

impl<'a> BinaryProbeCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            inner: Cursor::new(bytes),
        }
    }
}

impl Read for BinaryProbeCursor<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let position = self.inner.position();
        let read = self.inner.read(buffer)?;
        if position == 0 && read != 0 {
            buffer[0] = b'b';
        }
        Ok(read)
    }
}

impl Seek for BinaryProbeCursor<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(position)
    }
}

fn scale_vertices(
    vertices: [stl_io::Vertex; 3],
    scale: f32,
    triangle: usize,
) -> Result<[[f32; 3]; 3], StlLoadError> {
    let mut scaled = [[0.0; 3]; 3];
    for (vertex_index, vertex) in vertices.into_iter().enumerate() {
        for (component_index, coordinate) in vertex.0.into_iter().enumerate() {
            if !coordinate.is_finite() {
                return Err(StlLoadError::NonFiniteCoordinate {
                    triangle,
                    vertex: vertex_index,
                    component: component_index,
                });
            }
            let value = coordinate * scale;
            if !value.is_finite() {
                return Err(StlLoadError::ScaledCoordinateNotFinite {
                    triangle,
                    vertex: vertex_index,
                    component: component_index,
                });
            }
            scaled[vertex_index][component_index] = canonical_zero(value);
        }
    }
    Ok(scaled)
}

fn generated_normal(vertices: [[f32; 3]; 3], triangle: usize) -> Result<[f32; 3], StlLoadError> {
    let vertices = vertices.map(|vertex| vertex.map(f64::from));
    let first_edge = subtract(vertices[1], vertices[0]);
    let second_edge = subtract(vertices[2], vertices[0]);
    let cross = [
        first_edge[1] * second_edge[2] - first_edge[2] * second_edge[1],
        first_edge[2] * second_edge[0] - first_edge[0] * second_edge[2],
        first_edge[0] * second_edge[1] - first_edge[1] * second_edge[0],
    ];
    let squared_length = cross
        .iter()
        .map(|component| component * component)
        .sum::<f64>();
    if squared_length == 0.0 || !squared_length.is_finite() {
        return Err(StlLoadError::DegenerateTriangle { triangle });
    }
    let length = squared_length.sqrt();
    Ok(cross.map(|component| normalized_component_to_f32(component / length)))
}

fn normalized_component_to_f32(component: f64) -> f32 {
    debug_assert!(component.is_finite());
    debug_assert!((-1.0..=1.0).contains(&component));
    #[allow(
        clippy::cast_possible_truncation,
        reason = "unit-vector components are finite within [-1, 1], and canonical meshes use f32"
    )]
    let component = component as f32;
    canonical_zero(component)
}

fn subtract(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn canonical_zero(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}
