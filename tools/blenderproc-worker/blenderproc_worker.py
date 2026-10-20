import blenderproc as bproc

"""Persistent BlenderProc worker for the bop JSON-lines protocol v1."""

import json
import math
import sys
import time
from pathlib import Path

import bpy
import numpy as np
from mathutils import Matrix, Quaternion, Vector

PROTOCOL_VERSION = 1
ASSETS: dict[str, dict] = {}
CATALOG_FINGERPRINT: str | None = None
ENVIRONMENT_PATH: str | None = None
OUTPUTS_CONFIGURED = False


def blenderproc_version() -> str:
    package_root = Path(bproc.__file__).resolve().parent.parent
    candidates = sorted(package_root.glob("blenderproc-*.dist-info"))
    if candidates:
        return candidates[0].name.removeprefix("blenderproc-").removesuffix(".dist-info")
    return "unknown"


def reply(kind: str, **fields: object) -> None:
    payload = json.dumps({"protocol_version": PROTOCOL_VERSION, "type": kind, **fields}, separators=(",", ":"))
    print(f"BOP1\t{payload}", flush=True)


def checked(message: dict) -> None:
    if message.get("protocol_version") != PROTOCOL_VERSION:
        raise ValueError("incompatible protocol_version")


def transform_matrix(wire: dict) -> Matrix:
    translation = wire["translation"]
    x, y, z, w = wire["rotation_xyzw"]
    return Matrix.Translation(translation) @ Quaternion((w, x, y, z)).to_matrix().to_4x4()


def make_material(wire: dict, textures: list[dict]) -> bpy.types.Material:
    material = bpy.data.materials.new("bop-material")
    material.use_nodes = True
    principled = material.node_tree.nodes.get("Principled BSDF")
    principled.inputs["Base Color"].default_value = wire["base_color_factor"]
    principled.inputs["Metallic"].default_value = wire["metallic_factor"]
    principled.inputs["Roughness"].default_value = wire["roughness_factor"]
    binding = wire.get("base_color_texture")
    if binding is not None:
        texture = textures[binding["texture"]]
        width, height = texture["dimensions"]
        image = bpy.data.images.new("bop-texture", width=width, height=height, alpha=True)
        # glTF rows run top to bottom, Blender's bottom to top.
        pixels = np.asarray(texture["rgba8_srgb"], dtype=np.float32).reshape((height, width, 4)) / 255.0
        image.pixels.foreach_set(np.flipud(pixels).ravel())
        nodes, links = material.node_tree.nodes, material.node_tree.links
        node = nodes.new("ShaderNodeTexImage")
        node.image = image
        node.interpolation = "Closest" if texture["nearest"] else "Linear"
        # Blender has one extension mode for both axes; U decides.
        node.extension = {"repeat": "REPEAT", "mirror": "MIRROR", "clamp": "EXTEND"}[texture["wrap"][0]]
        uv = nodes.new("ShaderNodeUVMap")
        uv.uv_map = f"TEXCOORD_{binding['tex_coord_set']}"
        links.new(uv.outputs["UV"], node.inputs["Vector"])
        # glTF: base color = factor x texture.
        tint = nodes.new("ShaderNodeVectorMath")
        tint.name = "bop-tint"
        tint.operation = "MULTIPLY"
        tint.inputs[1].default_value = wire["base_color_factor"][:3]
        links.new(node.outputs["Color"], tint.inputs[0])
        links.new(tint.outputs["Vector"], principled.inputs["Base Color"])
        alpha = nodes.new("ShaderNodeMath")
        alpha.name = "bop-alpha"
        alpha.operation = "MULTIPLY"
        alpha.inputs[1].default_value = wire["base_color_factor"][3]
        links.new(node.outputs["Alpha"], alpha.inputs[0])
        links.new(alpha.outputs["Value"], principled.inputs["Alpha"])
    return material


def prepare(message: dict) -> None:
    global ASSETS, CATALOG_FINGERPRINT, ENVIRONMENT_PATH
    ENVIRONMENT_PATH = message.get("environment_path")
    clear_scene_instances()
    for asset in ASSETS.values():
        for mesh in asset["meshes"]:
            bpy.data.meshes.remove(mesh)
    for material in list(bpy.data.materials):
        if material.name.startswith("bop-material"):
            bpy.data.materials.remove(material)
    for image in list(bpy.data.images):
        if image.name.startswith("bop-texture"):
            bpy.data.images.remove(image)
    prepared: dict[str, dict] = {}
    for asset in message["assets"]:
        materials = [make_material(item, asset["textures"]) for item in asset["materials"]]
        meshes = []
        for primitive in asset["primitives"]:
            mesh = bpy.data.meshes.new(f"bop-{asset['asset_id']}")
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
            # Every mesh gets one slot, so an instance material can fill it per object.
            mesh.materials.append(None if material_index is None else materials[material_index])
            meshes.append(mesh)
        prepared[asset["asset_id"]] = {"meshes": meshes}
    ASSETS = prepared
    CATALOG_FINGERPRINT = message["catalog_fingerprint"]
    reply("prepared", request_id=message["request_id"], catalog_fingerprint=CATALOG_FINGERPRINT)


def clear_scene_instances() -> None:
    for obj in list(bpy.context.scene.objects):
        if obj.name.startswith(("instance-", "directional-", "point-", "bin-")):
            light = obj.data if isinstance(obj.data, bpy.types.Light) else None
            mesh = obj.data if obj.name.startswith("bin-") else None
            bpy.data.objects.remove(obj, do_unlink=True)
            if light is not None:
                bpy.data.lights.remove(light)
            if mesh is not None:
                bpy.data.meshes.remove(mesh)
    for material in list(bpy.data.materials):
        if material.name.startswith(("bin-material", "instance-material")):
            bpy.data.materials.remove(material)


def instance_material(file_material, wire: dict) -> bpy.types.Material:
    """The file material (or a plain one) with the instance's color factor, metallic and roughness."""
    if file_material is None:
        material = bpy.data.materials.new("instance-material")
        material.use_nodes = True
    else:
        material = file_material.copy()
        material.name = "instance-material"
    nodes = material.node_tree.nodes
    principled = nodes.get("Principled BSDF")
    red, green, blue, alpha = wire["base_color_srgb"]
    color = (*(srgb_to_linear(c) for c in (red, green, blue)), alpha)
    principled.inputs["Base Color"].default_value = color
    principled.inputs["Metallic"].default_value = wire["metallic"]
    principled.inputs["Roughness"].default_value = wire["roughness"]
    # A textured file material keeps its texture, tinted by the new color.
    if (tint := nodes.get("bop-tint")) is not None:
        tint.inputs[1].default_value = color[:3]
    if (alpha_node := nodes.get("bop-alpha")) is not None:
        alpha_node.inputs[1].default_value = alpha
    return material


def srgb_to_linear(value: float) -> float:
    return value / 12.92 if value <= 0.04045 else ((value + 0.055) / 1.055) ** 2.4


def bin_cuboids(bin_wire: dict) -> list[tuple[list[float], list[float]]]:
    """(half extents, bin-local centre) of the five cuboids, as in bop_assets::analytic_bin."""
    half_x, half_y, half_z = (value / 2.0 for value in bin_wire["inner_dimensions_m"])
    half_floor = bin_wire["floor_thickness_m"] / 2.0
    wall = bin_wire["wall_thickness_m"]
    half_wall = wall / 2.0
    outer_half_x, outer_half_y = half_x + wall, half_y + wall
    wall_x, wall_y = half_x + half_wall, half_y + half_wall
    return [
        ([outer_half_x, outer_half_y, half_floor], [0.0, 0.0, -half_floor]),
        ([half_wall, outer_half_y, half_z], [-wall_x, 0.0, half_z]),
        ([half_wall, outer_half_y, half_z], [wall_x, 0.0, half_z]),
        ([half_x, half_wall, half_z], [0.0, -wall_y, half_z]),
        ([half_x, half_wall, half_z], [0.0, wall_y, half_z]),
    ]


def add_bin(bin_wire: dict, number: int) -> None:
    """Draws the bin physics collided with; it stays background (instance id 0)."""
    surface = bin_wire["render_material"]
    material = bpy.data.materials.new(f"bin-material-{number}")
    material.use_nodes = True
    principled = material.node_tree.nodes.get("Principled BSDF")
    red, green, blue, alpha = surface["base_color_srgb"]
    principled.inputs["Base Color"].default_value = (*(srgb_to_linear(c) for c in (red, green, blue)), alpha)
    principled.inputs["Metallic"].default_value = surface["metallic"]
    principled.inputs["Roughness"].default_value = surface["roughness"]
    world_from_bin = transform_matrix(bin_wire["world_from_bin"])
    corners = [(sx, sy, sz) for sx in (-1, 1) for sy in (-1, 1) for sz in (-1, 1)]
    faces = [(0, 1, 3, 2), (4, 6, 7, 5), (0, 4, 5, 1), (2, 3, 7, 6), (0, 2, 6, 4), (1, 5, 7, 3)]
    for index, (half, centre) in enumerate(bin_cuboids(bin_wire)):
        vertices = [[centre[axis] + corner[axis] * half[axis] for axis in range(3)] for corner in corners]
        mesh = bpy.data.meshes.new(f"bin-{number}-{index}")
        mesh.from_pydata(vertices, [], faces)
        mesh.update()
        mesh.materials.append(material)
        obj = bpy.data.objects.new(f"bin-{number}-{index}", mesh)
        bpy.context.collection.objects.link(obj)
        obj.matrix_world = world_from_bin


def configure_scene(request: dict) -> None:
    clear_scene_instances()
    pass_index = 1
    materials = {item["instance_id"]: item for item in request.get("instance_materials", [])}
    for instance in request["scene"]["instances"]:
        chosen = materials.get(instance["instance_id"])
        for mesh in ASSETS[instance["asset_id"]]["meshes"]:
            obj = bpy.data.objects.new(f"instance-{instance['instance_id']}", mesh)
            bpy.context.collection.objects.link(obj)
            file_material = mesh.materials[0]
            if chosen is not None and (chosen["scope"] == "all" or file_material is None):
                slot = obj.material_slots[0]
                slot.link = "OBJECT"
                slot.material = instance_material(file_material, chosen)
            obj.matrix_world = transform_matrix(instance["world_from_model"])
            obj["bop_instance_id"] = instance["instance_id"]
            obj.pass_index = pass_index
            pass_index += 1
    for number, bin_wire in enumerate(request["scene"]["bins"]):
        add_bin(bin_wire, number)
    configure_world(request["appearance"])
    appearance = request["appearance"]
    bpy.context.scene.render.film_transparent = True
    for index, light in enumerate(appearance["directional_lights"]):
        data = bpy.data.lights.new(f"directional-{index}", "SUN")
        # Same scale as wgpu: 1000 lux lights a white diffuse surface facing the sun to 1.0;
        # Blender's diffuse BSDF divides sun irradiance by pi.
        data.energy = light["illuminance_lux"] / 1000.0 * math.pi
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


def configure_world(appearance: dict) -> None:
    """Lights the world with the environment map, or else the uniform ambient colour."""
    world = bpy.context.scene.world
    world.use_nodes = True
    nodes, links = world.node_tree.nodes, world.node_tree.links
    background = nodes.get("Background")
    for link in list(background.inputs["Color"].links):
        links.remove(link)
    environment = appearance.get("environment")
    if environment is None:
        background.inputs["Color"].default_value = (*appearance["ambient_light"]["color"], 1.0)
        background.inputs["Strength"].default_value = appearance["ambient_light"]["intensity"]
        return
    if ENVIRONMENT_PATH is None:
        raise ValueError("the scene uses an environment map but prepare sent none")
    texture = nodes.get("bop-environment") or nodes.new("ShaderNodeTexEnvironment")
    texture.name = "bop-environment"
    if texture.image is None or texture.image.filepath != ENVIRONMENT_PATH:
        texture.image = bpy.data.images.load(ENVIRONMENT_PATH, check_existing=True)
    mapping = nodes.get("bop-environment-mapping") or nodes.new("ShaderNodeMapping")
    mapping.name = "bop-environment-mapping"
    coordinates = nodes.get("bop-environment-coordinates") or nodes.new("ShaderNodeTexCoord")
    coordinates.name = "bop-environment-coordinates"
    # Turning the map by +rotation means sampling directions turned by -rotation.
    mapping.inputs["Rotation"].default_value = (0.0, 0.0, -math.radians(environment["rotation_deg"]))
    links.new(coordinates.outputs["Generated"], mapping.inputs["Vector"])
    links.new(mapping.outputs["Vector"], texture.inputs["Vector"])
    links.new(texture.outputs["Color"], background.inputs["Color"])
    background.inputs["Strength"].default_value = environment["intensity"]


def configure_camera(camera: dict) -> None:
    width, height = camera["image_size"]["width"], camera["image_size"]["height"]
    bproc.camera.set_intrinsics_from_K_matrix(np.asarray(camera["k"]).reshape((3, 3)), width, height)
    camera_from_world = transform_matrix(camera["camera_from_world"]["camera_from_world"])
    world_from_cv = camera_from_world.inverted()
    world_from_blender = world_from_cv @ Matrix.Diagonal((1.0, -1.0, -1.0, 1.0))
    camera_object = bpy.context.scene.camera
    camera_object.animation_data_clear()
    camera_object.matrix_world = world_from_blender
    camera_object.data.clip_start = camera["near_m"]
    camera_object.data.clip_end = camera["far_m"]
    bpy.context.scene.frame_start = 0
    bpy.context.scene.frame_end = 1
    bpy.context.scene.frame_set(0)


def direction_to_rotation(direction: np.ndarray):
    direction /= np.linalg.norm(direction)
    return Vector(direction.tolist()).to_track_quat("-Z", "Y").to_euler()


def render(message: dict) -> None:
    global OUTPUTS_CONFIGURED
    if CATALOG_FINGERPRINT is None:
        raise ValueError("prepare must precede render")
    request = message["request"]
    configure_scene(request)
    if not OUTPUTS_CONFIGURED:
        # RGBA output, so transparent film pixels get the configured background below.
        bproc.renderer.set_output_format(enable_transparency=True)
        bproc.renderer.enable_depth_output(activate_antialiasing=False)
        bproc.renderer.enable_segmentation_output(
            map_by=["bop_instance_id"],
            default_values={"bop_instance_id": 0},
        )
        OUTPUTS_CONFIGURED = True
    root = Path(message["output_root"])
    instance_key = "bop_instance_id_segmaps"
    started = time.perf_counter()
    for camera in request["cameras"]:
        configure_camera(camera)
        output = bproc.renderer.render()
        directory = root / "frames" / f"{camera['camera_id']:05d}"
        directory.mkdir(parents=True, exist_ok=True)
        color = np.asarray(output["colors"][0], dtype=np.float32)
        if color.shape[-1] == 4:
            alpha = color[..., 3:4] / 255.0
            background = np.asarray(request["appearance"]["background_srgb"][:3], dtype=np.float32) * 255.0
            color = color[..., :3] * alpha + background * (1.0 - alpha)
        np.clip(color[..., :3], 0.0, 255.0).astype(np.uint8).tofile(directory / "rgb.u8")
        depth = np.asarray(output["depth"][0], dtype="<f4")
        # Blender reports 1e10 where nothing is hit; the dataset convention (as in wgpu) is 0.
        depth = np.where(depth > camera["far_m"], np.float32(0.0), depth).astype("<f4")
        depth.tofile(directory / "depth.f32le")
        np.asarray(output[instance_key][0], dtype="<u2").tofile(directory / "instance.u16le")
    elapsed = time.perf_counter() - started
    reply("rendered", request_id=message["request_id"], scene_id=request["scene"]["scene_id"], render_seconds=elapsed)


def select_devices(selection: str, worker_index: int = 0, worker_count: int = 1) -> str:
    """Applies `device_selection` and returns the devices actually used.

    AUTO (or GPU) keeps BlenderProc's default order (OPTIX, CUDA, HIP; METAL on
    macOS) and falls back to the CPU, CPU renders on the CPU, and a type such as CUDA, optionally with GPU indices
    (CUDA:0 or CUDA:0,1), requires that type. When the run has more than one
    renderer worker, this worker keeps only GPU `worker_index` modulo the
    selected GPUs, so the workers spread over them instead of all sharing every GPU.
    """
    choice = selection.strip().upper()
    if choice in ("AUTO", "GPU"):
        bproc.renderer.set_render_devices()
    elif choice == "CPU":
        bproc.renderer.set_render_devices(use_only_cpu=True)
    else:
        kind, _, ids = choice.partition(":")
        if not bpy.context.preferences.addons["cycles"].preferences.get_devices_for_type(kind):
            raise RuntimeError(f"device_selection {selection!r}: no {kind} device is available")
        gpu_ids = [int(index) for index in ids.split(",")] if ids else None
        bproc.renderer.set_render_devices(desired_gpu_device_type=[kind], desired_gpu_ids=gpu_ids)
    if bpy.context.scene.cycles.device == "CPU":
        return "CPU"
    preferences = bpy.context.preferences.addons["cycles"].preferences
    # Cycles lists each GPU once per backend type; only the active type renders.
    used = [device for device in preferences.devices if device.use and device.type == preferences.compute_device_type]
    if not used:
        bpy.context.scene.cycles.device = "CPU"
        return "CPU"
    if worker_count > 1 and len(used) > 1:
        kept = used[worker_index % len(used)]
        for device in used:
            device.use = device is kept
        used = [kept]
    return f"{preferences.compute_device_type}: {', '.join(device.name for device in used)}"


def configure_sampling(message: dict) -> None:
    """Applies the configured samples, adaptive threshold and denoiser.

    Sampling is nearly all of a frame's cost. BlenderProc's own defaults (1024
    samples, OIDN as a CPU compositor node) are replaced: denoising runs inside
    Cycles, on the GPU where Blender supports it, and only touches RGB.
    """
    scene = bpy.context.scene
    bproc.renderer.set_denoiser(None)
    bproc.renderer.set_max_amount_of_samples(message["samples"])
    threshold = message["noise_threshold"]
    scene.cycles.use_adaptive_sampling = threshold > 0
    if threshold > 0:
        scene.cycles.adaptive_threshold = threshold
    # BlenderProc defaults to no glossy bounces: metals then reflect nothing.
    glossy = message["glossy_bounces"]
    bproc.renderer.set_light_bounces(glossy_bounces=glossy, max_bounces=max(3, glossy))
    denoiser = message["denoiser"]
    if denoiser not in ("none", "oidn", "optix"):
        raise ValueError(f"unknown denoiser {denoiser!r}")
    enabled = denoiser != "none"
    scene.cycles.use_denoising = enabled
    bpy.context.view_layer.cycles.use_denoising = enabled
    if enabled:
        scene.cycles.denoiser = "OPENIMAGEDENOISE" if denoiser == "oidn" else "OPTIX"
        scene.cycles.denoising_input_passes = "RGB_ALBEDO_NORMAL"
        scene.cycles.denoising_use_gpu = scene.cycles.device == "GPU"


def main() -> None:
    bproc.init()
    # wgpu writes linear radiance encoded as sRGB; Blender defaults to AgX tone mapping.
    bpy.context.scene.view_settings.view_transform = "Standard"
    for line in sys.stdin:
        try:
            message = json.loads(line)
            checked(message)
            kind = message["type"]
            if kind == "hello":
                implementation = f"blenderproc-{blenderproc_version()};blender-{bpy.app.version_string}"
                device = select_devices(
                    message["device_selection"], message.get("worker_index", 0), message.get("worker_count", 1)
                )
                configure_sampling(message)
                reply("ready", implementation_version=implementation, device_name=device)
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
