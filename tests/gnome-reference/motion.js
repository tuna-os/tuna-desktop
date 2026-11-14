// GNOME 51 window-motion reference for Tuna Desktop's paired animation gates.
// Run as: gnome-shell --headless --virtual-monitor 1280x800 --automation-script motion.js
// (scripts/tuna-gnome-reference --motion). Writes OUT/motion.json and
// OUT/<family>-<ms>.png.
//
// GNOME's own windowManager.js starts every effect (map, destroy, minimize,
// unminimize, size change, modal dim). Each ease() GNOME issues while a
// family is armed is paused at once; the sampler then seeks every held
// transition to the same exact delay (Clutter.Timeline.advance plus the
// transition's own new-frame handler) before each screenshot, so the frames
// are taken at fixed animation times however slowly llvmpipe paints, the way
// Tuna Desktop's proofs drive TUNA_ANIMATION_CLOCK. The sampler is generic:
// any family that eases actors can arm it with its own actor filter.
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Graphene from 'gi://Graphene';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import * as Config from 'resource:///org/gnome/shell/misc/config.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as Scripting from 'resource:///org/gnome/shell/ui/scripting.js';

Gio._promisify(Gio.Subprocess.prototype, 'wait_async');
Gio._promisify(Shell.Screenshot.prototype, 'screenshot');

const OUT = GLib.getenv('GREF_OUT') ?? '/out';
const COMMAND = `${GLib.get_tmp_dir()}/gref-lifecycle-command.json`;
const EASE_PARAMS = new Set(['duration', 'mode', 'delay', 'repeatCount',
    'autoReverse', 'animationRequired', 'onComplete', 'onStopped']);
// windowManager.js WINDOW_DIMMER_EFFECT_NAME and the property it eases.
const DIMMER = 'gnome-shell-window-dimmer';
const DIM_BRIGHTNESS = `@effects.${DIMMER}.brightness`;

// ---- Frozen-transition sampler -------------------------------------------
let armed = null;

function hold(actor, names) {
    for (const name of names) {
        const transition = actor.get_transition(name);
        if (!transition || armed.held.some(h => h.transition === transition))
            continue;
        transition.pause();
        armed.held.push({actor, name, transition, duration: transition.get_duration()});
    }
}

const ease = Clutter.Actor.prototype.ease;
Clutter.Actor.prototype.ease = function (props) {
    const result = ease.call(this, props);
    if (armed?.accept(this))
        hold(this, Object.keys(props).filter(k => !EASE_PARAMS.has(k)).map(k => k.replaceAll('_', '-')));
    return result;
};
const easeProperty = Clutter.Actor.prototype.ease_property;
Clutter.Actor.prototype.ease_property = function (name, target, params) {
    const result = easeProperty.call(this, name, target, params);
    if (armed?.accept(this))
        hold(this, [name]);
    return result;
};

function arm(accept) {
    armed = {accept, held: []};
}

async function held(ready, label, {optional = false} = {}) {
    for (let i = 0; i < (optional ? 20 : 200); i++) {
        if (ready(armed.held)) {
            // Companions GNOME starts a microtask later (the attached dialog
            // after its parent's dim, the window after its clone) are held
            // on creation too; give them a moment to be issued.
            await Scripting.sleep(150);
            return true;
        }
        await Scripting.sleep(50);
    }
    if (optional)
        return false;
    throw new Error(`GREF ${label}: GNOME started no matching effect (${armed.held.map(h => h.name)})`);
}

// Every held transition sits exactly `ms` into its own timeline (clamped
// to its duration), with the eased property set from Clutter's interval.
// The stage then paints twice, so the screenshot shows those values.
async function seek(ms) {
    for (const h of armed.held) {
        const at = Math.min(ms, h.duration);
        h.transition.advance(at);
        h.transition.emit('new-frame', at);
    }
    for (let i = 0; i < 2; i++) {
        await new Promise(resolve => {
            const id = global.stage.connect('after-paint', () => {
                global.stage.disconnect(id);
                resolve();
            });
            global.stage.queue_redraw();
        });
    }
}

async function release() {
    for (const h of armed.held)
        h.transition.start();
    armed = null;
    await Scripting.sleep(800);
}

// ---- Records ---------------------------------------------------------------
const windowActors = () => global.get_window_actors().filter(a => !a.is_destroyed?.());
const isClone = actor => global.get_window_actors().some(a => a.__animationInfo?.clone === actor);
// The role an actor first showed: a dying dialog loses its transient parent
// and a finished clone leaves __animationInfo, but they keep their role.
const roles = new WeakMap();
function role(actor) {
    if (!roles.has(actor)) {
        roles.set(actor, actor.meta_window
            ? (actor.meta_window.get_transient_for() ? 'dialog' : 'window')
            : isClone(actor) ? 'clone' : actor.constructor.$gtype.name);
    }
    return roles.get(actor);
}

function drawn(actor, rect) {
    // Where rect (in the actor's own coordinates) lands on the stage.
    const corners = [[rect[0], rect[1]], [rect[0] + rect[2], rect[1] + rect[3]]]
        .map(([x, y]) => actor.apply_transform_to_point(new Graphene.Point3D({x, y, z: 0})));
    return [corners[0].x, corners[0].y, corners[1].x - corners[0].x, corners[1].y - corners[0].y];
}

function record(actor) {
    const entry = {
        role: role(actor),
        opacity: actor.opacity,
        scale: [actor.scale_x, actor.scale_y],
        pivot: [actor.pivot_point.x, actor.pivot_point.y],
        translation: [actor.translation_x, actor.translation_y],
        allocation: [actor.x, actor.y, actor.width, actor.height],
        extents: drawn(actor, [0, 0, actor.width, actor.height]),
    };
    const win = actor.meta_window;
    if (win) {
        const frame = win.get_frame_rect();
        const buffer = win.get_buffer_rect();
        entry.title = win.get_title();
        entry.frame_rect = [frame.x, frame.y, frame.width, frame.height];
        entry.buffer_rect = [buffer.x, buffer.y, buffer.width, buffer.height];
        // The frame (without client shadows) as drawn this frame.
        entry.frame_drawn = drawn(actor, [frame.x - buffer.x, frame.y - buffer.y, frame.width, frame.height]);
    }
    const dim = actor.get_effect?.(DIMMER);
    if (dim) {
        const [r] = dim.get_brightness();
        entry.dim = {enabled: dim.enabled, brightness: r};
    }
    return entry;
}

const reference = {
    gnome_shell: Config.PACKAGE_VERSION,
    monitor: GLib.getenv('GREF_SIZE') ?? '1280x800',
    families: {},
};

async function screenshot(name) {
    const file = Gio.File.new_for_path(`${OUT}/${name}.png`);
    const stream = file.replace(null, false, Gio.FileCreateFlags.NONE, null);
    await new Shell.Screenshot().screenshot(false, stream);
    stream.close(null);
    return `${name}.png`;
}

function settled() {
    return windowActors().map(record);
}

async function sample(family, delays, extra = {}) {
    const samples = [];
    const actors = [...new Set(armed.held.map(h => h.actor))];
    for (const ms of delays) {
        await seek(ms);
        const png = await screenshot(`${family}-${String(ms).padStart(4, '0')}`);
        samples.push({t_ms: ms, png, actors: actors.map(record)});
    }
    reference.families[family] = {
        transitions: armed.held.map(h => ({role: role(h.actor), property: h.name, duration_ms: h.duration})),
        samples,
        ...extra,
    };
    await release();
    reference.families[family].settled = settled();
    reference.families[family].settled_png = await screenshot(`${family}-settled`);
    print(`GREF motion ${family}: ${samples.length} samples`);
    // Written after every family, so a failed run keeps what it measured.
    GLib.file_set_contents(`${OUT}/motion.json`, JSON.stringify(reference, null, 1));
}

// An effect GNOME skipped: what it drew straight away.
async function instant(family) {
    reference.families[family] = {
        transitions: armed.held.map(h => ({role: role(h.actor), property: h.name, duration_ms: h.duration})),
        settled: settled(),
        settled_png: await screenshot(`${family}-settled`),
    };
    armed = null;
    print(`GREF motion ${family}: instant`);
    GLib.file_set_contents(`${OUT}/motion.json`, JSON.stringify(reference, null, 1));
}

// ---- Driving the client ----------------------------------------------------
let generation = 0;
function command(action) {
    generation += 1;
    GLib.file_set_contents(COMMAND, JSON.stringify({action, generation}));
}

function spawnClient() {
    const launcher = new Gio.SubprocessLauncher({flags: Gio.SubprocessFlags.NONE});
    launcher.setenv('TUNA_LIFECYCLE_COMMAND', COMMAND, true);
    launcher.setenv('GDK_BACKEND', 'wayland', true);
    GLib.file_set_contents(COMMAND, JSON.stringify({action: 'none', generation: 0}));
    generation = 0;
    return launcher.spawnv(['/usr/bin/python3', '/proof-lib/tuna-window-lifecycle-client.py']);
}

let keyboard = null;
async function chord(keys) {
    keyboard ??= global.stage.context.get_backend().get_default_seat()
        .create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
    for (const key of keys) {
        keyboard.notify_keyval(GLib.get_monotonic_time(), key, Clutter.KeyState.PRESSED);
        await Scripting.sleep(80);
    }
    for (const key of [...keys].reverse()) {
        keyboard.notify_keyval(GLib.get_monotonic_time(), key, Clutter.KeyState.RELEASED);
        await Scripting.sleep(80);
    }
}

const normal = () => windowActors().find(a => a.meta_window.get_title() === 'Lifecycle normal');
const dialog = () => windowActors().find(a => a.meta_window.get_title() === 'Lifecycle dialog');
const anyWindowActor = actor => actor instanceof Meta.WindowActor;
const windowOrClone = actor => actor instanceof Meta.WindowActor || isClone(actor);
const has = (list, r, property) => list.some(h => role(h.actor) === r && (!property || h.name === property));

const MAP = [0, 10, 25, 50, 75, 150];
const DESTROY = [0, 25, 50, 75, 150];
const DIALOG_IN = [0, 25, 50, 100, 250, 500];
const DIALOG_OUT = [0, 25, 50, 100, 125, 250];
const MINIMIZE = [0, 25, 50, 100, 200, 400];
const SIZE = [0, 25, 62, 125, 187, 250];

async function mapNormal(family, delays) {
    arm(anyWindowActor);
    const client = spawnClient();
    await held(h => has(h, 'window', 'opacity'), family);
    await sample(family, delays);
    return client;
}

async function closeNormal(family, delays, client) {
    arm(anyWindowActor);
    command('close');
    await held(h => has(h, 'window', 'opacity'), family);
    await sample(family, delays);
    await client.wait_async(null);
}

async function dialogFamilies(prefix) {
    arm(anyWindowActor);
    command('dialog');
    // The dialog's own map effect and its parent's dim start together.
    await held(h => has(h, 'window', DIM_BRIGHTNESS), `${prefix}dialog-map`);
    await sample(`${prefix}dialog-map-dim`, DIALOG_IN, {attached: dialog()?.meta_window.is_attached_dialog() ?? null});
    arm(anyWindowActor);
    command('close-dialog');
    await held(h => has(h, 'window', DIM_BRIGHTNESS), `${prefix}dialog-destroy`);
    await sample(`${prefix}dialog-destroy-undim`, DIALOG_OUT);
}

async function minimizeFamilies(prefix) {
    const win = normal().meta_window;
    arm(anyWindowActor);
    command('minimize');
    await held(h => has(h, 'window', 'opacity'), `${prefix}minimize`);
    await sample(`${prefix}minimize`, MINIMIZE, {icon_geometry: win.get_icon_geometry()[0]});
    arm(anyWindowActor);
    if (!win.minimized)
        throw new Error(`GREF ${prefix}minimize: the window is not minimized`);
    win.activate(global.display.get_current_time_roundtrip());
    // GNOME 51 eases the reduced-motion (and monitor-sized) unminimize
    // fade on a still-hidden actor, so Clutter skips the implicit
    // transition and the window shows at once; record what GNOME draws.
    if (await held(h => has(h, 'window'), `${prefix}unminimize`, {optional: prefix !== ''}))
        await sample(`${prefix}unminimize`, MINIMIZE);
    else
        await instant(`${prefix}unminimize`);
}

async function sizeChange(family, trigger) {
    arm(windowOrClone);
    await trigger();
    // The clone of the old frame and the window both ease once the client
    // has drawn its new size (_sizeChangedWindow).
    await held(h => has(h, 'clone', 'opacity'), family);
    await sample(family, SIZE);
}

export async function run() {
    await Scripting.disableHelperAutoExit();
    Main.overview.hide();
    await Scripting.sleep(2000);
    reference.baseline_png = await screenshot('baseline');

    // Full motion.
    const client = await mapNormal('map', MAP);
    await dialogFamilies('');
    await minimizeFamilies('');
    for (const [family, keys] of [
        ['maximize', [Clutter.KEY_Super_L, Clutter.KEY_Up]],
        ['unmaximize', [Clutter.KEY_Super_L, Clutter.KEY_Down]],
        ['tile-left', [Clutter.KEY_Super_L, Clutter.KEY_Left]],
        ['untile-left', [Clutter.KEY_Super_L, Clutter.KEY_Down]],
        ['tile-right', [Clutter.KEY_Super_L, Clutter.KEY_Right]],
        ['untile-right', [Clutter.KEY_Super_L, Clutter.KEY_Down]],
    ])
        await sizeChange(family, () => chord(keys));
    await sizeChange('fullscreen', async () => command('fullscreen'));
    // A monitor-sized window only fades (windowManager.js:1174).
    await minimizeFamilies('fullscreen-');
    await sizeChange('unfullscreen', async () => command('unfullscreen'));
    await closeNormal('destroy', DESTROY, client);

    // Reduced motion: fades only (St.Settings reads this key live).
    const a11y = new Gio.Settings({schema_id: 'org.gnome.desktop.a11y.interface'});
    a11y.set_string('reduced-motion', 'reduce');
    await Scripting.sleep(500);
    const reduced = await mapNormal('reduced-map', MAP);
    await dialogFamilies('reduced-');
    await minimizeFamilies('reduced-');
    await sizeChange('reduced-maximize', () => chord([Clutter.KEY_Super_L, Clutter.KEY_Up]));
    await sizeChange('reduced-unmaximize', () => chord([Clutter.KEY_Super_L, Clutter.KEY_Down]));
    await closeNormal('reduced-destroy', DESTROY, reduced);
    a11y.reset('reduced-motion');

    // Animations off: GNOME starts no transition at all.
    const iface = new Gio.Settings({schema_id: 'org.gnome.desktop.interface'});
    iface.set_boolean('enable-animations', false);
    await Scripting.sleep(500);
    arm(anyWindowActor);
    const off = spawnClient();
    for (let i = 0; i < 100 && !normal(); i++)
        await Scripting.sleep(50);
    await Scripting.sleep(300);
    await instant('off-map');
    arm(anyWindowActor);
    command('close');
    await off.wait_async(null);
    await Scripting.sleep(300);
    await instant('off-destroy');
    iface.reset('enable-animations');

    GLib.file_set_contents(`${OUT}/motion.json`, JSON.stringify(reference, null, 1));
    print('GREF motion done');
}
