# game-tile: the look of the game's inventory tiles (ADR 0006). The default profile: a profile that leaves out a hook
# runs this one's (ADR 0042). Hooks take one context (render.py's Context: job, params, scene, paths, lap()).
#   before_open(ctx)          before the item's file opens (add-on constants, method replacements): nothing here
#   after_open(ctx)           over the materials and objects the add-on built: gloss, emission, screens, single meshes
#   stage(ctx)                the game's pose, camera fit, the game's light rig, studio world, render settings
#   post(ctx, exr_paths)      bloom, contrast, tone curve, the image file(s); returns their paths
import math
import os

import bpy
import numpy as np
from mathutils import Matrix, Vector

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


def before_open(ctx):
    pass


def after_open(ctx):
    sharpen_gloss()
    boost_emission()
    hide_screens(ctx.scene)
    unturn_single_mesh(ctx.scene)


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


def stage(ctx):
    """The game's pose (ctx.job pose, rig), the camera fit to the item, the light rig, the studio world and the render
    settings; without a pose a camera from az/el and three suns"""
    p, scene = ctx.params, ctx.scene
    pose, rig, size = ctx.job.get('pose'), ctx.job.get('rig'), p['size']
    if pose:
        # The game's pose: everything under an empty with Euler XYZ, camera on -Z, image-up = -Y
        holder = bpy.data.objects.new('icon_pose', None)
        scene.collection.objects.link(holder)
        holder.rotation_mode = 'XYZ'
        holder.rotation_euler = [math.radians(v) for v in pose]
        for o in [o for o in scene.objects if o.parent is None and o is not holder]:
            o.parent = holder
        direction = Vector((0.0, 0.0, 1.0 if p['mirror'] else -1.0))
        cam_rot = (0.0, 0.0, math.pi) if p['mirror'] else (math.pi, 0.0, 0.0)
    else:
        az, el = math.radians(p['az']), math.radians(p['el'])
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
    if pose and p['fit'] > 0:
        # The game scales the item into the cell; the rig (lights ~1 m from the centre, radius 2-3 m) is made
        # for that. A 3 m cooler would have the lights inside the model otherwise.
        holder.scale = [p['fit'] / max((hi - lo).x, (hi - lo).y, (hi - lo).z, 1e-6)] * 3
        bpy.context.view_layer.update()
        corners, lo, hi = bounds()
    center = (lo + hi) / 2
    ctx.info = f'bbox {tuple(round(v, 3) for v in (hi - lo))} m, {len(corners) // 8} meshes'

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
    half_tan = (cam_data.sensor_width / 2) / cam_data.lens / p['margin']
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
        import json
        with open(rig, encoding='utf-8') as f:
            lights = json.load(f)['lights']
        for l in lights:
            if not l['active'] or l['position'] is None:
                continue
            d = bpy.data.lights.new('rig_' + l['name'], 'SPOT')
            d.energy = l['intensity'] * p['light_scale']
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

    # Render: Eevee, transparent film; render.py writes it linear as a multi-layer EXR, post applies the tone curve
    # (the shoulder of Khronos PBR Neutral without its toe)
    scene.render.engine = 'BLENDER_EEVEE'
    scene.eevee.taa_render_samples = p['samples']
    # Glass (visors): the add-on turns on raytraced refraction per material, the scene has to follow; else Eevee
    # refracts only the grey world and the visor becomes a flat grey surface
    scene.eevee.use_raytracing = True
    scene.render.film_transparent = True
    # the emission pass: post lays the bloom over the glowing parts from it
    bpy.context.view_layer.use_pass_emit = True


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


def bloomed(exr, glow_exr=None):
    """Linear, premultiplied Combined pass plus the bloom from the emission pass (halos as in the game; the Artimex
    eye strip was a sharp line without it where the inventory tile lets it glow), as (linear rgb, alpha). With glass
    the emission comes from the second pass (glow_exr), in which the glass is see-through: else light behind glass is
    missing."""
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
    return lin, a


def save_image(rgba, out, file_format, colorspace, **settings):
    """(h, w, 4) rows from the top -> an image file through Blender's image writer"""
    h, w = rgba.shape[:2]
    dst = bpy.data.images.new('icon_out', w, h, alpha=True, float_buffer=file_format == 'OPEN_EXR')
    dst.colorspace_settings.name = colorspace
    # rows from the top, Blender pixels from the bottom
    dst.pixels.foreach_set(rgba[::-1].astype(np.float32).ravel())
    dst.filepath_raw = out
    dst.file_format = file_format
    if settings:
        # format settings (quality, depth) go through a scene's image settings
        # (the scene is set to the multi-layer EXR, which allows no other file format)
        scene = bpy.context.scene
        img = scene.render.image_settings
        old = (img.media_type, img.file_format, img.color_mode, img.quality, img.color_depth, img.exr_codec)
        img.media_type = 'IMAGE'
        img.file_format = file_format
        for k, v in settings.items():
            setattr(img, k, v)
        dst.save_render(out, scene=scene)
        img.media_type, img.file_format, img.color_mode, img.quality, img.color_depth, img.exr_codec = old
    else:
        dst.save()
    bpy.data.images.remove(dst)


def post(ctx, exr_paths):
    """The image in the profile's format from the render's EXR: png (sRGB, straight alpha: bloom, contrast(),
    shoulder()), jpeg (the same over the `background` colour, `quality`), exr (linear with bloom, premultiplied, no
    tone curve: for own grading). With glass a second pass for the emission (bloom behind glass)."""
    scene, fmt = ctx.scene, ctx.params['format']
    glow_exr = None
    try:
        if glass_see_through():
            # a second pass only for the emission pass (bloom behind glass), few samples: ~0.1-0.2 s
            scene.eevee.taa_render_samples = GLOW_SAMPLES
            bpy.ops.render.render(write_still=False)
            glow_exr = ctx.out + '.glow.exr'
            bpy.data.images['Render Result'].save_render(glow_exr, scene=scene)
        ctx.lap('glass')
        lin, a = bloomed(exr_paths[0], glow_exr)
    finally:
        if glow_exr and os.path.exists(glow_exr):
            os.remove(glow_exr)
    out = ctx.output_path(fmt)
    if fmt == 'exr':
        save_image(np.concatenate([lin, a], axis=-1), out, 'OPEN_EXR', 'Linear Rec.709', color_depth='16', exr_codec='ZIP')
        return [out]
    rgb = np.clip(shoulder(contrast(lin / np.maximum(a, 1e-6))), 0, 1)
    rgb = np.where(rgb <= 0.0031308, rgb * 12.92, 1.055 * rgb ** (1 / 2.4) - 0.055)
    if fmt == 'jpeg':
        bg = np.array(ctx.params['background'], dtype=np.float64).reshape(1, 1, 3)
        rgb = rgb * a + bg * (1 - a)  # sRGB values over the background, straight alpha
        save_image(np.concatenate([rgb, np.ones_like(a)], axis=-1), out, 'JPEG', 'Non-Color', color_mode='RGB', quality=int(ctx.params['quality']))
        return [out]
    # the values are sRGB-encoded already
    save_image(np.concatenate([rgb, a], axis=-1), out, 'PNG', 'Non-Color')
    return [out]
