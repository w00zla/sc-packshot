# Renders items as icons: bounding-box fit, the game's pose and light rig, transparent background.
# Runs only in Blender's own Python (no system Python), started by tools/packshot:
#   blender -b --python src/icons/render.py -- --stdin                     (persistent: one JSON job per stdin line)
#   blender -b --python src/icons/render.py -- --jobs <jobs.json>          (batch, one Blender process)
#   blender -b --python src/icons/render.py -- <in.glb|scene.blend> <out.png> [--pose 90,125,5] [--rig rig.json]
# --stdin prints "ready: pid <pid>" once Blender is up, then reads one job per line (the same object as one entry
# of a jobs file) and answers each with its icon:/failed: line; EOF or a line "quit" (or {"quit": true}) ends it.
# Input: a GLB (StarBreaker bundled) or the scene.blend of a decomposed export (the StarBreaker add-on then
# builds the materials). Pose = the game's defaultOrientation + category offset (degrees, Euler XYZ): the model
# is turned by it, the cell is then Y-up with the camera on -Z and image-up = -Y. Without a pose: camera from
# --az/--el (degrees about Z, 0 = camera on -X; height above the horizontal) and three suns.
import argparse
import json
import math
import os
import sys
import time
import traceback

import bpy
import numpy as np
from mathutils import Matrix, Vector


def parse_args():
    argv = sys.argv[sys.argv.index('--') + 1:] if '--' in sys.argv else []
    p = argparse.ArgumentParser()
    p.add_argument('src', nargs='?', type=os.path.abspath)
    p.add_argument('out', nargs='?', type=os.path.abspath)
    p.add_argument('--jobs', type=os.path.abspath, help='JSON list of {src, out, pose, rig, size}')
    p.add_argument('--stdin', action='store_true', help='persistent: one JSON job per stdin line, answered with its icon:/failed: line')
    p.add_argument('--size', type=int, default=512)
    p.add_argument('--pose', type=lambda s: [float(x) for x in s.split(',')], default=None)
    p.add_argument('--rig', type=os.path.abspath, default=None, help="the game's light rig (JSON from the CLI)")
    p.add_argument('--light-scale', type=float, default=30.0, help='watts per game intensity unit')
    p.add_argument('--fit', type=float, default=0.7, help='pose mode: the largest box edge in m the item is scaled to')
    p.add_argument('--mirror', action='store_true', help='camera on +Z instead of -Z (mirrors the image)')
    p.add_argument('--az', type=float, default=20.0)
    p.add_argument('--el', type=float, default=15.0)
    p.add_argument('--margin', type=float, default=1.06)
    p.add_argument('--samples', type=int, default=16)
    return p.parse_args(argv)


def load(path):
    if path.lower().endswith('.blend'):
        # StarBreaker decomposed: the add-on builds the materials on opening (at once in background mode)
        bpy.ops.wm.open_mainfile(filepath=path)
    else:
        bpy.ops.wm.read_factory_settings(use_empty=True)  # disables add-ons, hence only here
        bpy.ops.import_scene.gltf(filepath=path)
    return bpy.context.scene


def hide_screens(scene):
    # UI screens (material template screen_hud, e.g. the mobiGlas display) stay invisible without UI content
    # but spanned the box: the bracelet shrank to a dot. Hidden and left out of the fit, unless the item
    # consists of nothing else.
    def screen_only(o):
        used = {p.material_index for p in o.data.polygons}
        mats = [o.material_slots[i].material for i in used if i < len(o.material_slots)]
        return bool(mats) and all(m is not None and m.get('starbreaker_template_key') == 'screen_hud' for m in mats)
    meshes = [o for o in scene.objects if o.type == 'MESH' and o.data.polygons and not o.hide_render]
    screens = [o for o in meshes if screen_only(o)]
    if screens and len(screens) < len(meshes):
        for o in screens:
            o.hide_render = True
            o['icon_screen'] = True


def unturn_single_mesh(scene):
    # The game shows a model of exactly one mesh in the inventory in the mesh's own axes, without the rotation of
    # its NMC nodes: the FPS minerals (glacosite_fps, carinite_fps, …) carry +90° about X at the root and stood
    # upright here where the warehouse tiles show them lying (all 10 free tiles match without the rotation). The
    # fork keeps it: the same mesh is linked into assemblies there, where the rotation holds.
    bpy.context.view_layer.update()
    meshes = [o for o in scene.objects if o.type == 'MESH' and o.data.polygons and not o.hide_render]
    if len(meshes) != 1:
        return
    o = meshes[0]
    loc, rot, scale = o.matrix_world.decompose()
    if rot.angle < math.radians(0.5):
        return
    print(f'node rotation removed: {o.name} ({math.degrees(rot.angle):.0f}°)')
    o.parent = None
    o.matrix_world = Matrix.LocRotScale(loc, None, scale)


def boost_emission():
    # Glowing parts (eye strips, pupils, LEDs) times EMISSION_GAIN: at the data's strength the Artimex Chairman's
    # Club's eye strip was sRGB 126/64/33 against 196/79/39 in the inventory tile, the Dark Bear's pupils barely
    # showed. Every emission strength (linked ones too, also in node groups, once per group).
    done = set()

    def scale(tree, socket):
        if socket is None:
            return
        if not socket.is_linked:
            socket.default_value *= EMISSION_GAIN
            return
        link = socket.links[0]
        mul = tree.nodes.new('ShaderNodeMath')
        mul.operation = 'MULTIPLY'
        mul.inputs[1].default_value = EMISSION_GAIN
        tree.links.new(link.from_socket, mul.inputs[0])
        tree.links.remove(link)
        tree.links.new(mul.outputs[0], socket)

    def walk(tree):
        for n in list(tree.nodes):
            if n.bl_idname == 'ShaderNodeEmission':
                scale(tree, n.inputs['Strength'])
            elif n.bl_idname == 'ShaderNodeBsdfPrincipled':
                scale(tree, n.inputs.get('Emission Strength'))
            sub = getattr(n, 'node_tree', None)
            if sub is not None and sub.name not in done:
                done.add(sub.name)
                walk(sub)

    for m in bpy.data.materials:
        if m.node_tree is not None:
            walk(m.node_tree)


def sharpen_gloss():
    # Roughness of glossy surfaces (metal, glass) times ROUGHNESS: the Artimex Chairman's Club's gold stayed one even
    # tone (brightest pixels max Y 0.66, orange), the inventory tile has warm-white glints (max 0.95)
    done = set()

    def scale(tree, socket):
        if not socket.is_linked:
            socket.default_value *= ROUGHNESS
            return
        link = socket.links[0]
        mul = tree.nodes.new('ShaderNodeMath')
        mul.operation = 'MULTIPLY'
        mul.inputs[1].default_value = ROUGHNESS
        tree.links.new(link.from_socket, mul.inputs[0])
        tree.links.remove(link)
        tree.links.new(mul.outputs[0], socket)

    def walk(tree):
        for n in list(tree.nodes):
            if n.bl_idname == 'ShaderNodeBsdfPrincipled':
                metal, glass = n.inputs['Metallic'], n.inputs.get('Transmission Weight')
                if metal.is_linked or metal.default_value > 0 or (glass is not None and (glass.is_linked or glass.default_value > 0)):
                    scale(tree, n.inputs['Roughness'])
            sub = getattr(n, 'node_tree', None)
            if sub is not None and sub.name not in done:
                done.add(sub.name)
                walk(sub)

    for m in bpy.data.materials:
        if m.node_tree is not None:
            walk(m.node_tree)


def glass_see_through():
    """For the second pass: all glass (template SB_GlassPBR) see-through in its tint instead of refracting. Light
    behind visors and lenses then lands in the emission pass and gets the bloom (the Strata's padding lime behind
    the visor, the Dark Bear's pupils behind the lenses). False without glass."""
    found = False
    for g in bpy.data.node_groups:
        if not g.name.startswith('SB_GlassPBR'):
            continue
        out = next((n for n in g.nodes if n.bl_idname == 'NodeGroupOutput'), None)
        if out is None:
            continue
        clear = g.nodes.new('ShaderNodeBsdfTransparent')
        tint = next((n for n in g.nodes if n.bl_idname == 'ShaderNodeHueSaturation'), None)
        if tint is not None:
            g.links.new(tint.outputs[0], clear.inputs['Color'])
        g.links.new(clear.outputs[0], out.inputs[0])
        found = True
    return found


def mesh_corners(objects):
    for o in objects:
        if o.type != 'MESH' or not o.data.vertices or o.get('icon_screen'):
            continue
        for c in o.bound_box:
            yield o.matrix_world @ Vector(c)


def render(job, a):
    src, out = job['src'], job['out']
    pose, rig, size = job.get('pose'), job.get('rig'), job.get('size', a.size)
    # Phases per item (seconds) for the icon: line: load = opening the file incl. the add-on's material build,
    # prep = material tweaks, pose, camera, light, world, render = shader compilation and render incl. EXR,
    # glass = the second pass for glass, post = write_png
    t = [time.perf_counter()]
    scene = load(src)
    t.append(time.perf_counter())
    sharpen_gloss()
    boost_emission()
    hide_screens(scene)
    unturn_single_mesh(scene)

    if pose:
        # The game's pose: everything under an empty with Euler XYZ, camera on -Z, image-up = -Y
        holder = bpy.data.objects.new('icon_pose', None)
        scene.collection.objects.link(holder)
        holder.rotation_mode = 'XYZ'
        holder.rotation_euler = [math.radians(v) for v in pose]
        for o in [o for o in scene.objects if o.parent is None and o is not holder]:
            o.parent = holder
        direction = Vector((0.0, 0.0, 1.0 if a.mirror else -1.0))
        cam_rot = (0.0, 0.0, math.pi) if a.mirror else (math.pi, 0.0, 0.0)
    else:
        az, el = math.radians(a.az), math.radians(a.el)
        direction = Vector((-math.cos(el) * math.cos(az), -math.cos(el) * math.sin(az), math.sin(el)))
        cam_rot = direction.to_track_quat('Z', 'Y').to_euler()
    bpy.context.view_layer.update()

    def bounds():
        corners = list(mesh_corners(scene.objects))
        if not corners:
            raise RuntimeError('no meshes in the scene')
        lo = Vector((min(c.x for c in corners), min(c.y for c in corners), min(c.z for c in corners)))
        hi = Vector((max(c.x for c in corners), max(c.y for c in corners), max(c.z for c in corners)))
        return corners, lo, hi

    corners, lo, hi = bounds()
    if pose and a.fit > 0:
        # The game scales the item into the cell; the rig (lights ~1 m from the centre, radius 2-3 m) is made
        # for that. A 3 m cooler would have the lights inside the model otherwise.
        holder.scale = [a.fit / max((hi - lo).x, (hi - lo).y, (hi - lo).z, 1e-6)] * 3
        bpy.context.view_layer.update()
        corners, lo, hi = bounds()
    center = (lo + hi) / 2

    cam_data = bpy.data.cameras.new('icon_cam')
    cam_data.lens = 60
    cam_data.sensor_fit = 'AUTO'
    cam = bpy.data.objects.new('icon_cam', cam_data)
    scene.collection.objects.link(cam)
    scene.camera = cam
    cam.location = center
    cam.rotation_euler = cam_rot
    bpy.context.view_layer.update()  # else matrix_world is still the identity

    # Fit: back from the box centre until every corner is in the image, then centre on the box
    scene.render.resolution_x = scene.render.resolution_y = size
    scene.render.resolution_percentage = 100
    half_tan = (cam_data.sensor_width / 2) / cam_data.lens / a.margin
    inv = cam.matrix_world.inverted()
    push = 0.0
    for c in corners:
        v = inv @ c  # camera space: looking along -Z
        push = max(push, abs(v.x) / half_tan + v.z, abs(v.y) / half_tan + v.z)
    cam.location = center + direction * push
    bpy.context.view_layer.update()
    inv = cam.matrix_world.inverted()
    xs, ys = [], []
    for c in corners:
        v = inv @ c
        xs.append(v.x / -v.z)
        ys.append(v.y / -v.z)
    # centre the projection in the image (shift in units of the image width)
    cam_data.shift_x = ((max(xs) + min(xs)) / 2) / (2 * cam_data.sensor_width / 2 / cam_data.lens)
    cam_data.shift_y = ((max(ys) + min(ys)) / 2) / (2 * cam_data.sensor_width / 2 / cam_data.lens)

    cam_rot3 = cam.matrix_world.to_3x3()

    def sun(name, local_dir, energy, color=(1, 1, 1)):
        # a sun relative to the camera (x right, y up, z towards the camera)
        d = bpy.data.lights.new(name, 'SUN')
        d.energy = energy
        d.color = color
        d.angle = math.radians(15)
        o = bpy.data.objects.new(name, d)
        scene.collection.objects.link(o)
        v = (cam_rot3 @ Vector(local_dir)).normalized()  # direction from the object to the light
        o.rotation_euler = v.to_track_quat('Z', 'Y').to_euler()
        o.location = center + v * 10

    if rig:
        # The game's rig: sits at the cell centre, turned by (90, 180, 0); every projector shines along its local
        # +X axis. Positions relative to the item's box centre.
        rig_rot = Matrix.Rotation(math.radians(180), 3, 'Y') @ Matrix.Rotation(math.radians(90), 3, 'X')
        with open(rig, encoding='utf-8') as f:
            lights = json.load(f)['lights']
        for l in lights:
            if not l['active'] or l['position'] is None:
                continue
            d = bpy.data.lights.new('rig_' + l['name'], 'SPOT')
            d.energy = l['intensity'] * a.light_scale
            d.color = l['color']
            d.spot_size = math.radians(l['fov'] or 60)
            d.spot_blend = 0.5
            d.shadow_soft_size = l['bulb'] or 0.05
            d.use_shadow = bool(l['shadows'])
            o = bpy.data.objects.new('rig_' + l['name'], d)
            scene.collection.objects.link(o)
            o.location = center + rig_rot @ Vector(l['position'])
            rx, ry, rz = (math.radians(v) for v in l['rotation'])
            light_rot = Matrix.Rotation(rz, 3, 'Z') @ Matrix.Rotation(ry, 3, 'Y') @ Matrix.Rotation(rx, 3, 'X')
            beam = rig_rot @ light_rot @ Vector((1.0, 0.0, 0.0))
            o.rotation_euler = beam.to_track_quat('-Z', 'Y').to_euler()
    else:
        sun('key', (-0.6, 0.8, 0.7), 3.0)
        sun('fill', (0.8, 0.2, 0.6), 1.2, (0.9, 0.95, 1.0))
        sun('rim', (0.3, 0.6, -0.8), 2.0)
    world = bpy.data.worlds.new('icon_world')
    scene.world = world
    world.use_nodes = True
    nt = world.node_tree
    bg = nt.nodes['Background']
    bg.inputs['Color'].default_value = (1, 1, 1, 1)
    if pose:
        # A studio world instead of a grey sphere: Eevee lights diffuse and glossy from the world alike (one world
        # probe). A uniform grey laid a veil over black lacquer (Geist ASD, Golden Blossom), without a world
        # chrome reflected only black and had hard black sides (Goliath). A softbox above, faint light from the
        # camera side, almost black elsewhere; the rig's fill light is practically off (0.1).
        # Directions in the cell: image-up = -Y, towards the camera = -Z.
        tc = nt.nodes.new('ShaderNodeTexCoord')
        sep = nt.nodes.new('ShaderNodeSeparateXYZ')
        nt.links.new(tc.outputs['Generated'], sep.inputs[0])

        def band(axis, lo, hi, strength):
            # smoothstep over the negative axis: 0 at -axis = lo, strength at -axis = hi
            m = nt.nodes.new('ShaderNodeMapRange')
            m.interpolation_type = 'SMOOTHSTEP'
            m.inputs['From Min'].default_value = -lo
            m.inputs['From Max'].default_value = -hi
            m.inputs['To Min'].default_value = 0.0
            m.inputs['To Max'].default_value = strength
            nt.links.new(sep.outputs[axis], m.inputs['Value'])
            return m.outputs['Result']

        top = band('Y', STUDIO['top_from'], STUDIO['top_to'], STUDIO['top'])
        front = band('Z', 0.0, 0.9, STUDIO['front'])
        add = nt.nodes.new('ShaderNodeMath')
        add.operation = 'ADD'
        nt.links.new(top, add.inputs[0])
        nt.links.new(front, add.inputs[1])
        base = nt.nodes.new('ShaderNodeMath')
        base.operation = 'ADD'
        nt.links.new(add.outputs[0], base.inputs[0])
        base.inputs[1].default_value = STUDIO['base']
        nt.links.new(base.outputs[0], bg.inputs['Strength'])
    else:
        bg.inputs['Strength'].default_value = 0.15

    # Render: Eevee, transparent film, linear as EXR, write_png applies the tone curve (the shoulder of Khronos PBR
    # Neutral without its toe)
    scene.render.engine = 'BLENDER_EEVEE'
    scene.eevee.taa_render_samples = a.samples
    # Glass (visors): the add-on turns on raytraced refraction per material, the scene has to follow; else Eevee
    # refracts only the grey world and the visor becomes a flat grey surface
    scene.eevee.use_raytracing = True
    scene.render.film_transparent = True
    # multi-layer EXR with the emission pass: write_png lays the bloom over the glowing parts from it
    bpy.context.view_layer.use_pass_emit = True
    scene.render.image_settings.media_type = 'MULTI_LAYER_IMAGE'
    scene.render.image_settings.file_format = 'OPEN_EXR_MULTILAYER'
    scene.render.image_settings.color_depth = '32'
    scene.render.image_settings.color_mode = 'RGBA'
    scene.view_settings.view_transform = 'Standard'  # the EXR is written linear, the curve has no effect here
    scene.view_settings.look = 'None'
    scene.view_settings.exposure = 0.0
    t.append(time.perf_counter())
    bpy.ops.render.render(write_still=False)
    exr = out + '.exr'
    bpy.data.images['Render Result'].save_render(exr, scene=scene)
    t.append(time.perf_counter())
    glow_exr = None
    try:
        if glass_see_through():
            # a second pass only for the emission pass (bloom behind glass), few samples: ~0.1-0.2 s
            scene.eevee.taa_render_samples = GLOW_SAMPLES
            bpy.ops.render.render(write_still=False)
            glow_exr = out + '.glow.exr'
            bpy.data.images['Render Result'].save_render(glow_exr, scene=scene)
        t.append(time.perf_counter())
        write_png(exr, out, glow_exr)
        t.append(time.perf_counter())
    finally:
        os.remove(exr)
        if glow_exr and os.path.exists(glow_exr):
            os.remove(glow_exr)
    timing = ' '.join(f'{k}={b - a:.3f}' for k, a, b in zip(('load', 'prep', 'render', 'glass', 'post'), t, t[1:]))
    return f'bbox {tuple(round(v, 3) for v in (hi - lo))} m, {len(corners) // 8} meshes, timing {timing}'


# Studio world (luminance, white): softbox above from top_from to top_to (share of the image-up direction), camera
# side, base
STUDIO = {'top': 0.4, 'top_from': 0.2, 'top_to': 0.8, 'front': 0.05, 'base': 0.02}
# Factor on every emission strength (boost_emission)
EMISSION_GAIN = 4.0
# Bloom from the emission pass: strength and Gaussian radii (sigma in px at 512 px image size) with their weights
BLOOM = 2.0
BLOOM_BLURS = ((3.0, 0.6), (8.0, 0.4))
# Samples of the second pass (only emission behind glass, the bloom blurs it anyway)
GLOW_SAMPLES = 4
# Factor on the roughness of glossy surfaces (sharpen_gloss): between the probes 1.0 and 0.7, the light rig differs
# from the game's anyway (the tile shows which gold surfaces are bright there)
ROUGHNESS = 0.85
# Contrast of the luminance below mid-grey (exponent): black lacquer has a few percent under the key light and
# rendered dark grey without it (Geist ASD, ORC-mkX Singularity)
CONTRAST = 1.45


def shoulder(rgb):
    # Khronos PBR Neutral without its toe: linear up to 0.8, above that the brightest channel is compressed softly
    # towards 1 and slightly desaturated (light white keeps its detail, Snow Camo does not become a flat area). The
    # finished curve's toe subtracted min(R,G,B) - 6.25·min² from every channel and turned dark colours black or
    # garish (Geist Whiteout chin B/R 13 instead of 1.9, Rogue chin without blue); Standard clipped white hard, AgX
    # and Filmic turn white grey.
    start, desaturation = 0.8, 0.15
    peak = rgb.max(axis=-1, keepdims=True)
    d = 1 - start
    new_peak = 1 - d * d / (peak + d - start)
    comp = rgb * (new_peak / np.maximum(peak, 1e-9))
    g = 1 - 1 / (desaturation * (peak - new_peak) + 1)
    comp = comp * (1 - g) + new_peak * g
    return np.where(peak < start, rgb, comp)


def contrast(rgb):
    # Luminance Y below 0.18 -> 0.18·(Y/0.18)^CONTRAST, as one factor on every channel: shadows and black deeper, the
    # colour ratios stay (unlike the toe, which turned dark colours black). Unchanged above: over the whole range it
    # lifted the highlights too, white burnt out (Snow Camo 5.9 % of the pixels, CQ7 Snowfire 6.1 %)
    y = (0.2126 * rgb[..., 0:1] + 0.7152 * rgb[..., 1:2] + 0.0722 * rgb[..., 2:3])
    y = np.maximum(y, 1e-9)
    return rgb * np.where(y < 0.18, (y / 0.18) ** (CONTRAST - 1), 1.0)


def gauss_blur(img, sigma):
    """Separable Gaussian over (h, w, c), black outside."""
    r = max(1, int(3 * sigma))
    x = np.arange(-r, r + 1)
    k = np.exp(-x * x / (2 * sigma * sigma))
    k /= k.sum()
    pad = np.pad(img, ((r, r), (r, r), (0, 0)))
    rows = sum(k[i] * pad[i:i + img.shape[0]] for i in range(2 * r + 1))
    return sum(k[i] * rows[:, i:i + img.shape[1]] for i in range(2 * r + 1))


def read_layer(exr, layer):
    """One pass (Combined, Emission) of a multi-layer EXR as (h, w, c), rows from the top."""
    import OpenImageIO as oiio  # part of Blender's Python
    buf = oiio.ImageBuf(exr)
    spec = buf.spec()
    names = list(spec.channelnames)
    px = buf.get_pixels(oiio.FLOAT).reshape(spec.height, spec.width, -1).astype(np.float64)
    return px[..., [names.index(n) for c in 'RGBA' for n in names if n.endswith(f'.{layer}.{c}')]]


def write_png(exr, out, glow_exr=None):
    """Linear, premultiplied multi-layer EXR -> sRGB PNG with straight alpha: bloom from the emission pass (halos as
    in the game; the Artimex eye strip was a sharp line without it where the inventory tile lets it glow), then
    contrast() and shoulder(). With glass the emission comes from the second pass (glow_exr), in which the glass is
    see-through: else light behind glass is missing."""
    comb = read_layer(exr, 'Combined')
    emission = read_layer(glow_exr or exr, 'Emission')
    spec_width = comb.shape[1]
    lin, a = comb[..., :3], comb[..., 3:4]
    if emission.size and emission.max() > 0:
        scale = spec_width / 512
        bloom = BLOOM * sum(w * gauss_blur(emission, s * scale) for s, w in BLOOM_BLURS)
        lin = lin + bloom  # light, added premultiplied
        # the halo outside the item covers by its brightness
        glow = 0.2126 * bloom[..., 0:1] + 0.7152 * bloom[..., 1:2] + 0.0722 * bloom[..., 2:3]
        a = np.clip(a + (1 - a) * np.clip(glow * 4, 0, 1), 0, 1)
    rgb = np.clip(shoulder(contrast(lin / np.maximum(a, 1e-6))), 0, 1)
    rgb = np.where(rgb <= 0.0031308, rgb * 12.92, 1.055 * rgb ** (1 / 2.4) - 0.055)
    h, w = rgb.shape[:2]
    dst = bpy.data.images.new('icon_png', w, h, alpha=True)
    dst.colorspace_settings.name = 'Non-Color'  # the values are sRGB-encoded already
    # EXR rows from the top, Blender pixels from the bottom
    dst.pixels.foreach_set(np.concatenate([rgb, a], axis=-1)[::-1].astype(np.float32).ravel())
    dst.filepath_raw = out
    dst.file_format = 'PNG'
    dst.save()
    bpy.data.images.remove(dst)


def run_job(job, a):
    """One job: its icon: line, or its failed: line with the traceback on stderr; True when it rendered."""
    try:
        info = render(job, a)
        print(f"icon: {job['out']} {info}", flush=True)
        return True
    except Exception as e:  # a broken item does not abort the batch
        traceback.print_exc()
        sys.stderr.flush()
        print(f"failed: {job['out']} {e}", flush=True)
        return False


def serve(a):
    """--stdin: Blender starts once; one JSON job per line (as in a jobs file), each starting from a clean state
    like the next job of a jobs file (load() opens its file); EOF or quit ends it."""
    print(f'ready: pid {os.getpid()}', flush=True)
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        if line == 'quit':
            break
        try:
            job = json.loads(line)
        except ValueError as e:
            print(f'bad job line: {e}: {line[:200]}', file=sys.stderr, flush=True)
            continue
        if not isinstance(job, dict) or job.get('quit'):
            break
        run_job(job, a)


def share_path_indexes():
    """Batch and --stdin mode: the add-on keeps one path index per export root for every file opened (it re-checks
    a hit and rescans once on a miss); off by default for interactive use, absent in older add-ons."""
    try:
        from starbreaker_addon import manifest
    except ImportError:
        return
    if hasattr(manifest, 'share_path_indexes'):
        manifest.share_path_indexes(True)


def main():
    a = parse_args()
    if a.stdin or a.jobs:
        share_path_indexes()
    if a.stdin:
        serve(a)
        return
    if a.jobs:
        with open(a.jobs, encoding='utf-8') as f:
            jobs = json.load(f)
    elif a.src and a.out:
        jobs = [{'src': a.src, 'out': a.out, 'pose': a.pose, 'rig': a.rig, 'size': a.size}]
    else:
        sys.exit('Usage: render.py <src> <out> | --jobs <jobs.json> | --stdin')
    failed = 0
    for job in jobs:
        if not run_job(job, a):
            failed += 1
    if failed:
        sys.exit(1)


main()
