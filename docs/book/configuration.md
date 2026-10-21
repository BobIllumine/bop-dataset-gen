# Configuration

A run is described by one strict TOML file. Unknown fields are rejected, and
every value is validated before anything runs. `bop-gen schema` prints the full
JSON Schema; `schemas/run-config.schema.json` is the committed copy, and CI
fails if the two drift apart.

## Sections

| Section | Controls |
|---|---|
| `[run]` | Master seed, number of valid scenes, attempt limits, worker counts, queue capacity, and the asset cache path. |
| `[bin]` | Inner dimensions, wall and floor thickness, pose, contact properties, and rendered surface of the bin. For several bins use `[[bins]]` instead (see below). |
| `[[assets]]` | Each object model: source file, unit scale, model transform, mixture weight, physical-property distributions, and convex-decomposition parameters. |
| `[population]` | How many objects a scene gets, which assets they are drawn from, and the spawn volume. With `[[bins]]`, each bin has its own `[bins.population]`. |
| `[physics]` | Rapier timestep, solver settings, and the settling and rejection thresholds. |
| `[[cameras]]` | Image size, clip planes, projection, and a fixed or sampled pose. |
| `[appearance]` | Background, ambient light or environment map, directional and point lights, and default materials. |
| `[renderers.wgpu]`, `[renderers.blenderproc]` | Backend-specific settings. |
| `[output]` | Which channels to write (RGB, depth, visible instances) and how hard to compress PNGs. |

`examples/presets/` has four complete, commented configurations to start from;
see [Benchmarks](benchmarks.md).

## Camera poses

A `fixed_world_from_camera` pose is given in a Z-up camera frame: X right,
Y forward (the viewing direction), Z up. The identity pose therefore looks
along world +Y. To look straight down, turn −90° about X:
`rotation_xyzw = [-0.70710678, 0.0, 0.0, 0.70710678]`. Shell poses always look
at `target_bin_m`. Everything written to the dataset (`frame.json`) uses the
OpenCV convention instead: +X right, +Y down, +Z forward.

## Materials

Each object's surface is decided in this order:

1. `[[assets]] visual_override`, when the asset has one, replaces the file's
   base color factor, metallic and roughness on every part of the mesh. A
   base-color texture stays and is tinted by the new color.
2. Otherwise the asset file's own material is used as is. A GLB's
   metallic-roughness material (base color factor and texture, metallic,
   roughness) renders the same in both renderers.
3. Parts the file gives no material, such as every STL, use
   `[appearance] material_defaults`.

`metallic` and `roughness` are distributions, sampled once per object from
the scene's appearance seed, so a rerun reproduces them:

```toml
[[assets]]
id = "cup"
# ...
visual_override = { base_color_srgb = [0.8, 0.1, 0.1, 1.0], metallic = { kind = "fixed", value = 1.0 }, roughness = { kind = "uniform", min = 0.2, max = 0.3 } }

[appearance]
material_defaults = { base_color_srgb = [0.7, 0.7, 0.72, 1.0], metallic = { kind = "uniform", min = 0.0, max = 0.6 }, roughness = { kind = "uniform", min = 0.3, max = 0.8 } }
```

The values each object got are recorded in the scene's `materials.json`.

The bin's surface is set separately and defaults to light grey:

```toml
[bin]
render_material = { base_color_srgb = [0.6, 0.6, 0.62, 1.0], metallic = 0.0, roughness = 0.6 }
```

Distributions are written as tables such as
`{ kind = "fixed", value = 0.5 }`; the schema lists every kind.

## Environment maps

An equirectangular HDR image can light the scene instead of the uniform
ambient light, in both renderers:

```toml
[appearance]
environment = { path = "skies/studio.hdr", intensity = 1.0, rotation_deg = 0.0 }
```

- `path` is a 2:1 `.hdr` or `.exr`, relative to the configuration file (on
  the server, a file in the asset library). The image is mapped as Blender
  maps it: its centre faces world +X, a quarter to the left faces +Y.
- `intensity` scales its radiance; `rotation_deg` turns it about world +Z,
  counter-clockwise seen from above.
- `ambient` is still required but unused while an environment is set.
- The map lights diffuse surfaces and is what glossy surfaces reflect where
  nothing nearer is in the way. The camera background stays
  `background_srgb`.

## Output

`[output] png_compression` sets how hard RGB and instance PNGs are compressed:
`fast` (default), `balanced` or `high`. The pixels are identical at every
level. On rendered 640×480 frames `fast` encodes about 13 times faster than
`balanced` for files about 30% larger. Since depth is ZIP-compressed, RGB
is more than half of a wgpu dataset, so the level matters more for its size
than it did.

## Several bins

Replace `[bin]` and `[population]` with one `[[bins]]` entry per bin. Each
entry has the `[bin]` fields, a unique `id`, and its own population:

```toml
[[bins]]
id = "left"
inner_dimensions_m = [0.8, 0.6, 0.5]
world_from_bin = { translation = [-0.43, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
# ...the other [bin] fields

[bins.population]
object_count = { kind = "fixed", value = 16 }
# Mostly cups, with an occasional nippel that ended up in the wrong box.
asset_mixture = { kind = "choice", values = [{ value = "cup", weight = 0.95 }, { value = "nippel", weight = 0.05 }] }
spawn_volume = { min_m = [-0.33, -0.23, 0.08], max_m = [0.33, 0.23, 0.48] }
```

- **Spawn volume:** each bin's spawn volume is in that bin's own coordinates.
- **Instance IDs:** objects are numbered across bins in configuration order.
  With the same seed, the first bin gets the same objects a single-bin
  configuration would.
- **Overlap:** bins may not overlap. The check compares the outer boxes'
  world-axis-aligned bounds, so rotated bins need some clearance.
- **Escapes:** an object must settle in the bin it was dropped into, or the
  attempt is rejected as an escape.
- **Object limit:** the visible-instance limit of 65 535 counts all bins
  together.

Every object records its bin as `bin_id` in `scene.json`. The BOP export
repeats it in `scene_gt_bop.json`. A configuration written with `[bin]` has
one bin, `"bin"`. `examples/cup_nippel/two_bins.toml` is a complete
two-box example.

## Parallelism settings

```toml
[run]
simulation_workers = 8   # Rapier worlds simulating scenes in parallel
renderer_workers = 4     # renderer instances, each on its own thread
queue_capacity = 16      # simulated scenes allowed to wait for a renderer
```

Worker counts never change the output; see [Concurrency](concurrency.md).
Each renderer worker is a full renderer instance: one wgpu device, or one
BlenderProc process. On a machine with several GPUs, the workers spread over
them in turn: worker 0 on the first GPU, worker 1 on the second, and so on, so
set `renderer_workers` to at least the number of GPUs. See
[Multiple GPUs](renderers.md#multiple-gpus).

## Command-line overrides

`simulate`, `render`, and `generate` accept `--seed N` and `--scenes N`. An
override is written into the *resolved configuration*, the exact bytes stored
beside the output and used for provenance and resume compatibility. Without
overrides, the resolved configuration is the file's bytes unchanged.

Because a corpus and the datasets rendered from it must share one resolved
configuration, pass the same overrides to `bop-gen render` that you passed to
`bop-gen simulate`. Output paths are deliberately not part of the resolved
configuration, so a corpus can be rendered into any directory.
