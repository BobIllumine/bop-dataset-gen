"""Persistent BlenderProc worker for the bpp JSON-lines protocol v1."""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

import blenderproc as bproc
import bpy
import numpy as np
from mathutils import Matrix, Quaternion, Vector

PROTOCOL_VERSION = 1
ASSETS: dict[str, dict] = {}
CATALOG_FINGERPRINT: str | None = None


def reply(kind: str, **fields: object) -> None:
    print(json.dumps({"protocol_version": PROTOCOL_VERSION, "type": kind, **fields}, separators=(",", ":")), flush=True)


def checked(message: dict) -> None:
    if message.get("protocol_version") != PROTOCOL_VERSION:
        raise ValueError("incompatible protocol_version")


def transform_matrix(wire: dict) -> Matrix:
    translation = wire["translation"]
    x, y, z, w = wire["rotation_xyzw"]
    return Matrix.Translation(translation) @ Quaternion((w, x, y, z)).to_matrix().to_4x4()


def make_material(wire: dict, textures: list[dict]) -> bpy.types.Material:
    material = bpy.data.materials.new("bpp-material")
    material.use_nodes = True
    principled = material.node_tree.nodes.get("Principled BSDF")
    principled.inputs["Base Color"].default_value = wire["base_color_factor"]
    principled.inputs["Metallic"].default_value = wire["metallic_factor"]
    principled.inputs["Roughness"].default_value = wire["roughness_factor"]
    binding = wire.get("base_color_texture")
    if binding is not None:
        texture = textures[binding["texture"]]
        width, height = texture["dimensions"]
        image = bpy.data.images.new("bpp-texture", width=width, height=height, alpha=True)
        pixels = np.asarray(texture["rgba8_srgb"], dtype=np.float32).reshape((-1, 4)) / 255.0
        image.pixels.foreach_set(pixels.ravel())
        node = material.node_tree.nodes.new("ShaderNodeTexImage")
        node.image = image
        uv = material.node_tree.nodes.new("ShaderNodeUVMap")
        uv.uv_map = f"TEXCOORD_{binding['tex_coord_set']}"
        material.node_tree.links.new(uv.outputs["UV"], node.inputs["Vector"])
        material.node_tree.links.new(node.outputs["Color"], principled.inputs["Base Color"])
        material.node_tree.links.new(node.outputs["Alpha"], principled.inputs["Alpha"])
    return material


def prepare(message: dict) -> None:
    global ASSETS, CATALOG_FINGERPRINT
    prepared: dict[str, dict] = {}
    for asset in message["assets"]:
        materials = [make_material(item, asset["textures"]) for item in asset["materials"]]
        meshes = []
        for primitive in asset["primitives"]:
            mesh = bpy.data.meshes.new(f"bpp-{asset['asset_id']}")
            indices = primitive["indices"]
            faces = [indices[offset : offset + 3] for offset in range(0, len(indices), 3)]
            mesh.from_pydata(primitive["positions"], [], faces)
            mesh.update()
            mesh.normals_split_custom_set_from_vertices(primitive["normals"])
            for polygon in mesh.polygons:
                polygon.use_smooth = True
            for set_index, coordinates in enumerate(primitive["tex_coord_sets"]):
                layer = mesh.uv_layers.new(name=f"TEXCOORD_{set_index}")
                for loop in mesh.loops:
                    u, v = coordinates[loop.vertex_index]
                    layer.data[loop.index].uv = (u, 1.0 - v)
            material_index = primitive.get("material")
            if material_index is not None:
                mesh.materials.append(materials[material_index])
            meshes.append(mesh)
        prepared[asset["asset_id"]] = {"meshes": meshes}
    ASSETS = prepared
    CATALOG_FINGERPRINT = message["catalog_fingerprint"]
    reply("prepared", request_id=message["request_id"], catalog_fingerprint=CATALOG_FINGERPRINT)


def clear_scene_instances() -> None:
    for obj in list(bpy.context.scene.objects):
        bpy.data.objects.remove(obj, do_unlink=True)
    bproc.camera.clear_camera_poses()


def configure_scene(request: dict) -> None:
    clear_scene_instances()
    for instance in request["scene"]["instances"]:
        for mesh in ASSETS[instance["asset_id"]]["meshes"]:
            obj = bpy.data.objects.new(f"instance-{instance['instance_id']}", mesh)
            bpy.context.collection.objects.link(obj)
            obj.matrix_world = transform_matrix(instance["world_from_model"])
            obj["bpp_instance_id"] = instance["instance_id"]
    appearance = request["appearance"]
    world = bpy.context.scene.world
    world.use_nodes = True
    background = world.node_tree.nodes.get("Background")
    background.inputs["Color"].default_value = (*appearance["ambient_light"]["color"], 1.0)
    background.inputs["Strength"].default_value = appearance["ambient_light"]["intensity"]
    for index, light in enumerate(appearance["directional_lights"]):
        data = bpy.data.lights.new(f"directional-{index}", "SUN")
        data.energy = light["illuminance_lux"]
        data.color = light["color"]
        obj = bpy.data.objects.new(data.name, data)
        bpy.context.collection.objects.link(obj)
        direction = np.asarray(light["direction_world"], dtype=np.float64)
        obj.rotation_euler = direction_to_rotation(direction)
    for index, light in enumerate(appearance["point_lights"]):
        data = bpy.data.lights.new(f"point-{index}", "POINT")
        data.energy = light["luminous_power_lm"]
        data.color = light["color"]
        obj = bpy.data.objects.new(data.name, data)
        bpy.context.collection.objects.link(obj)
        obj.location = light["position_world_m"]


def configure_camera(camera: dict) -> None:
    bproc.camera.clear_camera_poses()
    width, height = camera["image_size"]["width"], camera["image_size"]["height"]
    bproc.camera.set_intrinsics_from_K_matrix(np.asarray(camera["k"]).reshape((3, 3)), width, height)
    camera_from_world = transform_matrix(camera["camera_from_world"]["camera_from_world"])
    world_from_cv = camera_from_world.inverted()
    world_from_blender = world_from_cv @ Matrix.Diagonal((1.0, -1.0, -1.0, 1.0))
    bproc.camera.add_camera_pose(np.asarray(world_from_blender))
    bpy.context.scene.camera.data.clip_start = camera["near_m"]
    bpy.context.scene.camera.data.clip_end = camera["far_m"]


def direction_to_rotation(direction: np.ndarray):
    direction /= np.linalg.norm(direction)
    return Vector(direction.tolist()).to_track_quat("-Z", "Y").to_euler()


def render(message: dict) -> None:
    if CATALOG_FINGERPRINT is None:
        raise ValueError("prepare must precede render")
    request = message["request"]
    configure_scene(request)
    bproc.renderer.enable_depth_output(activate_antialiasing=False)
    bproc.renderer.enable_segmentation_output(map_by=["cp_bpp_instance_id"])
    root = Path(message["output_root"])
    instance_key = "cp_bpp_instance_id_segmaps"
    started = time.perf_counter()
    for camera in request["cameras"]:
        configure_camera(camera)
        output = bproc.renderer.render()
        directory = root / "frames" / f"{camera['camera_id']:05d}"
        directory.mkdir(parents=True, exist_ok=True)
        np.asarray(output["colors"][0], dtype=np.uint8)[..., :3].tofile(directory / "rgb.u8")
        np.asarray(output["depth"][0], dtype="<f4").tofile(directory / "depth.f32le")
        np.asarray(output[instance_key][0], dtype="<u2").tofile(directory / "instance.u16le")
    elapsed = time.perf_counter() - started
    reply("rendered", request_id=message["request_id"], scene_id=request["scene"]["scene_id"], render_seconds=elapsed)


def main() -> None:
    bproc.init()
    for line in sys.stdin:
        try:
            message = json.loads(line)
            checked(message)
            kind = message["type"]
            if kind == "hello":
                reply("ready", implementation_version="blenderproc", device_name=message["device_selection"])
            elif kind == "prepare":
                prepare(message)
            elif kind == "render":
                render(message)
            elif kind == "flush":
                reply("flushed", request_id=message["request_id"])
            elif kind == "shutdown":
                return
            else:
                raise ValueError(f"unknown message type {kind}")
        except Exception as error:  # Blender failures must cross the protocol boundary.
            reply("error", request_id=message.get("request_id", 0), code=type(error).__name__, message=str(error))


if __name__ == "__main__":
    main()
