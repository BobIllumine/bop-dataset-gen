# BOP export

`bop-gen export bop DATASET --output DIRECTORY` converts a complete canonical
dataset into the [BOP 6D-pose format](https://github.com/thodan/bop_toolkit/blob/master/docs/bop_datasets_format.md).
The dataset is validated first; an incomplete or tampered dataset is refused
with exit code 5. `DIRECTORY` must not exist. The export is written to a
hidden sibling directory, validated, and renamed into place, so `DIRECTORY`
either does not exist or holds a complete export.

## Layout

```text
bop/
├── models/
│   ├── models_info.json        diameter, min and size per object, millimetres
│   └── obj_000001.ply          binary PLY, millimetres, with vertex normals
├── camera.json                 only when every camera has the same K and size
├── train_pbr/
│   └── 000000/                 one BOP scene per canonical scene
│       ├── scene_camera.json   cam_K, depth_scale, cam_R_w2c, cam_t_w2c (mm)
│       ├── scene_gt.json       cam_R_m2c, cam_t_m2c (mm), obj_id
│       ├── scene_gt_bop.json   instance_id and bin_id of each scene_gt entry
│       ├── rgb/000001.png      image ID = camera ID
│       ├── depth/000001.png    16-bit depth
│       └── mask_visib/000001_000000.png
├── bop_objects.json            obj_id → asset ID and content fingerprint
└── bop_export.json             split, depth scale, source run manifest
```

The `bop_*.json` files and `scene_gt_bop.json` are not part of BOP:

- **Provenance:** the two `bop_*.json` files.
- **Bin labels:** `scene_gt_bop.json` follows `scene_gt.json` entry for
  entry. It says which canonical instance each ground-truth entry is, and
  which bin it was dropped into.

BOP tools ignore all of them.

## Mapping

- **Objects.** Assets are numbered 1, 2, … in asset-ID order. The models come
  from the dataset's own `assets/` directory, the canonical meshes the scenes
  were generated with, scaled from metres to millimetres.
- **Scenes and images.** Each canonical scene becomes one BOP scene with the
  same number. Each camera becomes one image whose `im_id` is the camera ID.
  BOP scene directories have six digits, so scene IDs above 999 999 are
  refused.
- **Poses.** `cam_R_m2c` and `cam_t_m2c` are `camera_from_world ∘
  world_from_model`, computed in f64 from the stored f32 poses, in the OpenCV
  camera convention BOP uses. Rotations are row-major.
- **Ground truth order.** `scene_gt.json` lists every instance in instance-ID
  order, including instances no camera sees. Mask `IIIIII_GGGGGG.png` belongs
  to entry `GGGGGG` of image `IIIIII`.
- **Depth.** Canonical depth is f32 camera-Z metres. It is quantized to 16-bit
  units of `depth_scale` millimetres, rounded to nearest. The default scale is
  0.1 mm, which covers 6.5535 m, as in the BOP PBR training sets. Use
  `--depth-scale MM` for longer ranges. A pixel that does not fit is an error,
  never a clamp. Background stays 0.
- **RGB** is copied unchanged.

## Not exported

- Amodal masks (`mask/`) and `scene_gt_info.json` need a separate render of
  each object without occluders, so they are left out. `bop_export.json`
  lists them under `omitted`.
- Only the `train_pbr` split is written.

## Checks

`validate_bop` runs on every export before the rename. It checks that every
model listed in `models_info.json` exists, that `scene_camera.json` and
`scene_gt.json` cover the same images, that intrinsics are finite, depth scales
positive, and rotations proper, that object IDs are known, and that the RGB,
depth, and mask images of one view share one size, with one mask per
ground-truth entry.

The test suite also recomputes every exported pose independently and checks
depth and masks against the canonical frames. On wgpu datasets, projecting each
model with the exported `cam_K`, `cam_R_m2c` and `cam_t_m2c` lands every visible
mask pixel inside the projected model, and the 16-bit depth inside the model's
depth range. The official `bop_toolkit` readers load the models, ground truth,
cameras, and depth.

Comparing an export against BlenderProc's own BOP writer has not been done
yet; it needs a BlenderProc installation.
