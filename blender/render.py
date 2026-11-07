# Renders items with a profile (ADRs 0022, 0041, 0042). Runs only in Blender's own Python (no system Python), started
# by packshot:
#   blender -b --python blender/render.py -- --stdin [--profile <dir>]                (persistent: one JSON job per line)
#   blender -b --python blender/render.py -- --jobs <jobs.json> [--profile <dir>]     (batch, one Blender process)
#   blender -b --python blender/render.py -- <in.glb|scene.blend> <out.png> [--pose 90,125,5] [--rig rig.json]
# --stdin prints "ready: pid <pid>" once Blender is up, then reads one job per line (the same object as one entry
# of a jobs file) and answers each with its icon:/failed: line; EOF or a line "quit" (or {"quit": true}) ends it.
# Input: a GLB (StarBreaker bundled) or the scene.blend of a decomposed export (the StarBreaker add-on then builds
# the materials on opening). This file is the core: it opens the item, calls the profile's hooks, renders a linear
# multi-layer EXR and hands it to the profile's post hook, which writes the output. The look lives in the profile
# (profile.toml + hooks.py, by default profiles/default); a hook the profile leaves out runs the default profile's.
# Parameters: profile.toml [params], then this script's flags, then the job's fields. Size and format are the target
# set's (ADR 0045): packshot puts them into every job; a profile does not set them, by hand they default to 256 png.
# profile.toml [addon] (ADR 0049): `override` (the profile's own add-on copy) and the add-on's look knobs, constants of
# starbreaker_addon.runtime.constants checked once at the start and set on it before every item's file opens.
import argparse
import importlib.util
import json
import os
import re
import sys
import time
import tomllib
import traceback

import bpy

HOOKS = ('before_open', 'after_open', 'stage', 'post')
DEFAULT_PROFILE = 'default'
# size and format without a job's or a flag's: render.py used by hand (packshot always sends the target set's)
OUTPUT = {'size': 256, 'format': 'png'}


def parse_args():
    argv = sys.argv[sys.argv.index('--') + 1:] if '--' in sys.argv else []
    p = argparse.ArgumentParser()
    p.add_argument('src', nargs='?', type=os.path.abspath)
    p.add_argument('out', nargs='?', type=os.path.abspath)
    p.add_argument('--jobs', type=os.path.abspath, help='JSON list of {src, out, pose, rig, size}')
    p.add_argument('--stdin', action='store_true', help='persistent: one JSON job per stdin line, answered with its icon:/failed: line')
    p.add_argument('--profile', type=os.path.abspath, default=None, help='profile folder (default: profiles/default)')
    p.add_argument('--pose', type=lambda s: [float(x) for x in s.split(',')], default=None)
    p.add_argument('--rig', type=os.path.abspath, default=None, help="the game's light rig (JSON from packshot)")
    # parameters: given here, they override the profile's
    p.add_argument('--size', type=int)
    p.add_argument('--samples', type=int)
    p.add_argument('--format', choices=('png', 'jpeg', 'exr'))
    p.add_argument('--light-scale', type=float, help='watts per game intensity unit')
    p.add_argument('--fit', type=float, help='pose mode: the largest box edge in m the item is scaled to')
    p.add_argument('--mirror', action='store_true', default=None, help='camera on +Z instead of -Z (mirrors the image)')
    p.add_argument('--az', type=float)
    p.add_argument('--el', type=float)
    p.add_argument('--margin', type=float)
    return p.parse_args(argv)


def profile_dirs():
    """Where a profile named by its folder name is looked for: next to this script (a render chunk's inbox, which
    packshot fills with the profiles it sends) and in the repository / bundle (blender/ or cli/ beside profiles/)"""
    here = os.path.dirname(os.path.abspath(__file__))
    return [os.path.join(here, 'profiles'), os.path.join(os.path.dirname(here), 'profiles')]


def find_default():
    for d in profile_dirs():
        if os.path.isfile(os.path.join(d, DEFAULT_PROFILE, 'profile.toml')):
            return os.path.join(d, DEFAULT_PROFILE)
    sys.exit(f'profile {DEFAULT_PROFILE} not found in {" or ".join(profile_dirs())}')


def load_module(folder, name):
    path = os.path.join(folder, 'hooks.py')
    if not os.path.isfile(path):
        return None
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Profile:
    """A profile folder: its parameters (default's under its own) and its hooks (default's where it has none)"""

    def __init__(self, folder, a):
        default = find_default()
        folder = folder or default
        with open(os.path.join(default, 'profile.toml'), 'rb') as f:
            base = tomllib.load(f)
        with open(os.path.join(folder, 'profile.toml'), 'rb') as f:
            own = tomllib.load(f)
        self.folder = folder
        self.name = os.path.basename(folder)
        for p in (base, own):
            bad = [k for k in OUTPUT if k in p.get('params', {})]
            if bad:
                sys.exit(f'profile.toml of {folder if p is own else default} sets {", ".join(bad)}: size and format belong to the set')
        self.params = {**OUTPUT, **base.get('params', {}), **own.get('params', {})}
        flags = {'size': a.size, 'samples': a.samples, 'format': a.format, 'light_scale': a.light_scale, 'fit': a.fit,
                 'mirror': a.mirror, 'az': a.az, 'el': a.el, 'margin': a.margin}
        self.params.update({k: v for k, v in flags.items() if v is not None})
        self.scene = os.path.join(folder, own['scene']) if own.get('scene') else None
        # [addon] (ADR 0049): `override` (its own add-on copy) and the add-on's look knobs; 1.0.0's top-level
        # `addon = "..."` is the override alone
        table = own.get('addon')
        table = table if isinstance(table, dict) else {'override': table} if table else {}
        override = table.get('override')
        bad = [k for k in table if k != 'override' and not KNOB_NAME.match(k)]
        if bad:
            sys.exit(f'profile.toml of {folder}, [addon]: unknown option {", ".join(bad)} '
                     "(override, or an add-on constant's UPPER_CASE name)")
        if override is not None and not isinstance(override, str):
            sys.exit(f"profile.toml of {folder}, [addon]: override is the add-on folder's name, a string")
        # its own copy of the StarBreaker add-on (ADR 0025), active before its hooks load: they may import it
        self.addon = os.path.join(folder, override) if override else None
        if self.addon:
            use_addon(self.addon)
        self.knobs = check_knobs({k: v for k, v in table.items() if k != 'override'}, folder)
        defaults = load_module(default, 'packshot_profile_default')
        mine = defaults if os.path.samefile(folder, default) else load_module(folder, 'packshot_profile')
        self.hooks = {h: getattr(mine, h, None) or getattr(defaults, h) for h in HOOKS}


class Context:
    """What a hook gets: the job, the parameters (profile, flags, job), the scene once it is open, the paths, and
    lap() to time a phase (load, prep, render, glass, post in the icon: line)"""

    def __init__(self, job, profile):
        self.job = job
        self.params = {**profile.params, **{k: job[k] for k in ('size', 'format') if job.get(k) is not None}}
        self.scene = None
        self.src, self.out = job['src'], job['out']
        self.profile = profile.folder
        self.scene_blend = profile.scene
        self.info = ''
        self.laps = {}
        self._t = time.perf_counter()

    def lap(self, name):
        now = time.perf_counter()
        self.laps[name] = self.laps.get(name, 0.0) + now - self._t
        self._t = now

    def output_path(self, fmt):
        """The job's output path with the extension of `fmt` (png, jpeg -> .jpg, exr)"""
        return os.path.splitext(self.out)[0] + {'jpeg': '.jpg'}.get(fmt, '.' + fmt)

    def use_scene_blend(self):
        """Links the world, cameras and lights of the profile's scene .blend (profile.toml `scene`, `scene_blend` here)
        into the item's scene; its world and first camera become the scene's. For a stage hook that takes them from it."""
        if not self.scene_blend:
            return
        with bpy.data.libraries.load(self.scene_blend, link=False) as (src, dst):
            dst.worlds = src.worlds
            dst.objects = [n for n in src.objects]
        for o in dst.objects:
            if o is not None and o.type in ('CAMERA', 'LIGHT'):
                self.scene.collection.objects.link(o)
                if o.type == 'CAMERA' and self.scene.camera is None:
                    self.scene.camera = o
        if dst.worlds:
            self.scene.world = dst.worlds[0]


def load(path):
    if path.lower().endswith('.blend'):
        # StarBreaker decomposed: the add-on builds the materials on opening (at once in background mode)
        bpy.ops.wm.open_mainfile(filepath=path)
    else:
        bpy.ops.wm.read_factory_settings(use_empty=True)  # disables add-ons, hence only here
        bpy.ops.import_scene.gltf(filepath=path)
    return bpy.context.scene


def render(job, profile):
    """One item through the hooks; returns (output files, info for the icon: line)"""
    ctx = Context(job, profile)
    hooks = profile.hooks
    apply_knobs(profile.knobs)
    hooks['before_open'](ctx)
    ctx.scene = load(ctx.src)
    ctx.lap('load')
    hooks['after_open'](ctx)
    hooks['stage'](ctx)
    # the core's contract with post: a linear multi-layer EXR of every pass the stage turned on
    scene = ctx.scene
    scene.render.image_settings.media_type = 'MULTI_LAYER_IMAGE'
    scene.render.image_settings.file_format = 'OPEN_EXR_MULTILAYER'
    scene.render.image_settings.color_depth = '32'
    scene.render.image_settings.color_mode = 'RGBA'
    scene.view_settings.view_transform = 'Standard'  # the EXR is written linear, the curve has no effect here
    scene.view_settings.look = 'None'
    scene.view_settings.exposure = 0.0
    ctx.lap('prep')
    bpy.ops.render.render(write_still=False)
    exr = ctx.out + '.exr'
    bpy.data.images['Render Result'].save_render(exr, scene=scene)
    ctx.lap('render')
    try:
        outputs = hooks['post'](ctx, [exr])
        ctx.lap('post')
    finally:
        os.remove(exr)
    timing = ' '.join(f'{k}={ctx.laps.get(k, 0.0):.3f}' for k in ('load', 'prep', 'render', 'glass', 'post'))
    return outputs, f'{ctx.info}, timing {timing}' if ctx.info else f'timing {timing}'


def run_job(job, profile):
    """One job: its icon: line (the first output file), or its failed: line with the traceback on stderr; True when
    it rendered."""
    try:
        outputs, info = render(job, profile)
        print(f"icon: {outputs[0] if outputs else job['out']} {info}", flush=True)
        return True
    except Exception as e:  # a broken item does not abort the batch
        traceback.print_exc()
        sys.stderr.flush()
        print(f"failed: {job['out']} {e}", flush=True)
        return False


def serve(profile):
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
        run_job(job, profile)


ADDON = 'starbreaker_addon'


def use_addon(folder):
    """A profile's own add-on copy (profile.toml `[addon] override` or 1.0.0's `addon`, a folder named
    starbreaker_addon inside the profile) instead of the installed one, for the whole process (a Blender process belongs to one profile): the installed one is
    unregistered and its modules dropped, the copy's parent goes first on sys.path and Blender enables the copy under
    the same name (its preferences entry stays), before the first file opens."""
    import addon_utils
    if os.path.basename(os.path.normpath(folder)) != ADDON:
        sys.exit(f'profile add-on {folder}: the folder must be called {ADDON}')
    if not os.path.isfile(os.path.join(folder, '__init__.py')):
        sys.exit(f'profile add-on {folder}: no __init__.py in it')
    addon_utils.disable(ADDON)
    for m in [m for m in sys.modules if m == ADDON or m.startswith(ADDON + '.')]:
        del sys.modules[m]
    sys.path.insert(0, os.path.dirname(os.path.normpath(folder)))
    importlib.invalidate_caches()
    mod = addon_utils.enable(ADDON)
    if mod is None or not os.path.normcase(os.path.abspath(mod.__file__)).startswith(os.path.normcase(os.path.abspath(folder))):
        sys.exit(f'profile add-on {folder} did not load (got {getattr(mod, "__file__", None)})')
    print(f'add-on: {folder}', file=sys.stderr, flush=True)


CONSTANTS = ADDON + '.runtime.constants'
# the names [addon] may set besides override: the add-on's constants (upper case)
KNOB_NAME = re.compile(r'[A-Z][A-Z0-9_]*\Z')


def is_number(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def fit_knob(value, cur):
    """A profile's [addon] value as the constant's type: (value, None), or (None, why) when the types differ (int and
    float are both numbers, a bool is none; a list for a tuple or list of the same length, item by item; a list
    becomes a tuple for a tuple)"""
    if isinstance(cur, bool):
        return (value, None) if isinstance(value, bool) else (None, f'the constant is a bool ({cur!r})')
    if is_number(cur):
        if not is_number(value):
            return None, f'the constant is a number ({cur!r})'
        return (float(value) if isinstance(cur, float) else value), None
    if isinstance(cur, str):
        return (value, None) if isinstance(value, str) else (None, f'the constant is a string ({cur!r})')
    if isinstance(cur, (tuple, list)):
        if not isinstance(value, list):
            return None, f'the constant is a {type(cur).__name__} ({cur!r})'
        if len(value) != len(cur):
            return None, f'{len(value)} values, the constant has {len(cur)} ({cur!r})'
        items = []
        for v, c in zip(value, cur):
            x, why = fit_knob(v, c)
            if why:
                return None, f'item {len(items) + 1}: {why}'
            items.append(x)
        return (tuple(items) if isinstance(cur, tuple) else items), None
    return None, f'the constant is a {type(cur).__name__}, no TOML value replaces it'


def check_knobs(knobs, folder):
    """The profile's add-on knobs (ADR 0049) against the active add-on's runtime/constants.py (the installed one or
    the profile's copy): {name: value as the constant's type}; a name the module does not define or a value of
    another type ends the process here, before any job"""
    if not knobs:
        return {}
    try:
        mod = importlib.import_module(CONSTANTS)
    except ImportError as e:
        sys.exit(f'profile.toml of {folder}, [addon]: the add-on has no runtime/constants.py ({e})')
    out, bad = {}, []
    for name, value in knobs.items():
        if not hasattr(mod, name):
            bad.append(f'{name}: the add-on defines no such constant')
            continue
        v, why = fit_knob(value, getattr(mod, name))
        if why:
            bad.append(f'{name} = {value!r}: {why}')
        else:
            out[name] = v
    if bad:
        sys.exit(f'profile.toml of {folder}, [addon] ({mod.__file__}): ' + '; '.join(bad))
    print(f'add-on knobs: {", ".join(f"{k} = {v!r}" for k, v in out.items())}', file=sys.stderr, flush=True)
    return out


def apply_knobs(knobs):
    """Before every item's file opens (and before before_open): the knobs on the add-on's constants module, as it is
    imported now"""
    if knobs:
        mod = importlib.import_module(CONSTANTS)
        for name, value in knobs.items():
            setattr(mod, name, value)


def share_path_indexes():
    """Batch and --stdin mode: the add-on keeps one path index per export root for every file opened (it re-checks
    a hit and rescans once on a miss); off by default for interactive use, absent in older add-ons. Imported by name:
    the installed add-on or the profile's copy, whichever is active."""
    try:
        manifest = importlib.import_module(ADDON + '.manifest')
    except ImportError:
        return
    if hasattr(manifest, 'share_path_indexes'):
        manifest.share_path_indexes(True)


def main():
    a = parse_args()
    profile = Profile(a.profile, a)
    if a.stdin or a.jobs:
        share_path_indexes()
    if a.stdin:
        serve(profile)
        return
    if a.jobs:
        with open(a.jobs, encoding='utf-8') as f:
            jobs = json.load(f)
    elif a.src and a.out:
        jobs = [{'src': a.src, 'out': a.out, 'pose': a.pose, 'rig': a.rig}]
    else:
        sys.exit('Usage: render.py <src> <out> | --jobs <jobs.json> | --stdin')
    failed = 0
    for job in jobs:
        if not run_job(job, profile):
            failed += 1
    if failed:
        sys.exit(1)


main()
