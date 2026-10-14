import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const MAX_OFFSET = 7;
const EFFECT_RADIUS = 2.25;
const MIN_ICON_SIZE = 16;
const MAX_ICON_SIZE = 320;
const ICON_SCALE = 1.12;
// Dock/overview actors are rebuilt between pointer events; never reuse a stale actor.
const ACTOR_SCAN_INTERVAL_US = 0;

export default class LiquidGlassParallaxExtension extends Extension {
    enable() {
        this._managedFile = Gio.File.new_for_path(GLib.build_filenamev([
            GLib.get_user_data_dir(),
            'liquid-glass-icon',
            'managed-icons.json',
        ]));
        this._managedIds = new Set();
        this._actors = new Map();
        this._iconActors = new Map();
        this._compositions = new Set();
        this._destroyIds = new Map();
        this._nextActorScan = 0;
        this._reloadManagedIds();
        this._monitor = this._managedFile.monitor_file(Gio.FileMonitorFlags.NONE, null);
        this._monitorId = this._monitor.connect('changed', () => this._reloadManagedIds());
        this._motionId = global.stage.connect('captured-event', (_stage, event) => {
            if (event.type() === Clutter.EventType.MOTION) {
                const [x, y] = event.get_coords();
                this._applyParallax(x, y);
            }
            return Clutter.EVENT_PROPAGATE;
        });
    }

    disable() {
        if (this._motionId)
            global.stage.disconnect(this._motionId);
        if (this._monitorId)
            this._monitor.disconnect(this._monitorId);
        this._monitor?.cancel();
        this._restoreAll();
        for (const [actor, signalId] of this._destroyIds ?? []) {
            try {
                actor.disconnect(signalId);
            } catch (_) {
                // The Shell may have disposed the actor already.
            }
        }
        this._actors = null;
        this._iconActors = null;
        this._compositions = null;
        this._destroyIds = null;
        this._managedIds = null;
        this._monitor = null;
        this._motionId = 0;
        this._monitorId = 0;
    }

    _reloadManagedIds() {
        const managedIds = new Set();
        try {
            if (this._managedFile.query_exists(null)) {
                const [, bytes] = this._managedFile.load_contents(null);
                for (const desktopId of Object.keys(JSON.parse(new TextDecoder().decode(bytes)).entries ?? {}))
                    managedIds.add(desktopId);
            }
        } catch (error) {
            console.warn(`Liquid Glass parallax: could not read managed icon state: ${error.message}`);
        }
        this._managedIds = managedIds;
        this._nextActorScan = 0;
        this._restoreAll();
    }

    _applyParallax(pointerX, pointerY) {
        this._refreshIconActors();
        const currentActors = new Set();
        for (const [actor, desktopId] of this._iconActors) {
            let x;
            let y;
            let width;
            let height;
            try {
                [x, y] = actor.get_transformed_position();
                [width, height] = actor.get_transformed_size();
            } catch (_) {
                this._restore(actor);
                continue;
            }
            if (width < MIN_ICON_SIZE || height < MIN_ICON_SIZE || width > MAX_ICON_SIZE || height > MAX_ICON_SIZE) {
                this._restore(actor);
                continue;
            }
            const composition = this._ensureComposition(actor, desktopId, x, y, width, height);
            if (!composition) {
                this._restore(actor);
                continue;
            }
            currentActors.add(actor);
            composition.overlay.set_position(
                x - (width * ICON_SCALE - width) / 2,
                y - (height * ICON_SCALE - height) / 2,
            );
            composition.overlay.set_size(width * ICON_SCALE, height * ICON_SCALE);
            const radius = Math.max(width, height) * EFFECT_RADIUS;
            const dx = (pointerX - (x + width / 2)) / radius;
            const dy = (pointerY - (y + height / 2)) / radius;
            const weight = Math.max(0, 1 - Math.hypot(dx, dy));
            if (weight === 0) {
                this._restore(actor);
                continue;
            }
            for (const layer of composition.layers) {
                const base = this._remember(layer);
                currentActors.add(layer);
                layer.set_translation(
                    base.x + dx * MAX_OFFSET * weight,
                    base.y + dy * MAX_OFFSET * weight,
                    base.z,
                );
            }
        }
        for (const actor of [...this._actors.keys()]) {
            if (!currentActors.has(actor))
                this._restore(actor);
        }
    }

    _refreshIconActors() {
        const now = GLib.get_monotonic_time();
        if (now < this._nextActorScan)
            return;
        this._iconActors = new Map(this._managedIconActors());
        this._nextActorScan = now + ACTOR_SCAN_INTERVAL_US;
    }

    _ensureComposition(actor, desktopId, x, y, width, height) {
        if (this._compositions.has(actor))
            return actor._liquidGlassComposition;
        const directory = GLib.build_filenamev([
            GLib.get_user_data_dir(),
            'liquid-glass-icon',
            'parallax',
            this._outputName(desktopId),
        ]);
        const baseFile = Gio.File.new_for_path(GLib.build_filenamev([directory, 'base.png']));
        if (!baseFile.query_exists(null))
            return null;
        let originalOpacity;
        try {
            originalOpacity = actor.opacity;
            const overlay = new St.Widget({
                layout_manager: new Clutter.BinLayout(),
                reactive: false,
            });
            global.stage.add_child(overlay);
            overlay.set_position(
                x - (width * ICON_SCALE - width) / 2,
                y - (height * ICON_SCALE - height) / 2,
            );
            overlay.set_size(width * ICON_SCALE, height * ICON_SCALE);
            const base = new St.Icon({
                gicon: Gio.FileIcon.new(baseFile),
                x_expand: true,
                y_expand: true,
                x_align: Clutter.ActorAlign.FILL,
                y_align: Clutter.ActorAlign.FILL,
            });
            base.reactive = false;
            base._liquidGlassParallaxLayer = true;
            overlay.add_child(base);
            const layers = [];
            for (let index = 1; index <= 16; index++) {
                const file = Gio.File.new_for_path(
                    GLib.build_filenamev([directory, `layer-${index}.png`]),
                );
                if (!file.query_exists(null))
                    break;
                const layer = new St.Icon({
                    gicon: Gio.FileIcon.new(file),
                    x_expand: true,
                    y_expand: true,
                    x_align: Clutter.ActorAlign.FILL,
                    y_align: Clutter.ActorAlign.FILL,
                });
                layer.reactive = false;
                layer._liquidGlassParallaxLayer = true;
                overlay.add_child(layer);
                layers.push(layer);
            }
            if (layers.length === 0) {
                overlay.destroy();
                return null;
            }
            actor.opacity = 0;
            actor._liquidGlassComposition = {
                overlay,
                originalOpacity,
                layers,
            };
            this._compositions.add(actor);
            return actor._liquidGlassComposition;
        } catch (error) {
            try {
                if (originalOpacity !== undefined)
                    actor.opacity = originalOpacity;
            } catch (_) {
                // The Shell may dispose the actor while its icon is rebuilt.
            }
            console.warn(`Liquid Glass parallax: could not compose ${desktopId}: ${error.message}`);
            return null;
        }
    }

    _outputName(desktopId) {
        const stem = desktopId.endsWith('.desktop') ? desktopId.slice(0, -8) : desktopId;
        const output = [...stem].map(character =>
            /[A-Za-z0-9_.-]/.test(character) ? character : '_');
        return output.join('') || 'application';
    }

    *_managedIconActors() {
        const pending = [[global.stage, null]];
        const seen = new Set();
        while (pending.length > 0) {
            const [actor, inheritedDesktopId] = pending.pop();
            if (!actor || seen.has(actor))
                continue;
            seen.add(actor);
            if (actor._liquidGlassParallaxLayer)
                continue;
            const app = actor._delegate?.app ?? actor._delegate?._app;
            const desktopId = app?.get_id?.() ?? inheritedDesktopId;
            if (actor instanceof St.Icon && desktopId && this._managedIds.has(desktopId)) {
                this._watchActor(actor);
                yield [actor, desktopId];
            }
            for (const child of actor.get_children())
                pending.push([child, desktopId]);
        }
    }

    _watchActor(actor) {
        if (this._destroyIds.has(actor))
            return;
        try {
            const signalId = actor.connect('destroy', () => {
                const composition = actor._liquidGlassComposition;
                composition?.overlay?.destroy();
                this._iconActors?.delete(actor);
                this._actors?.delete(actor);
                this._compositions?.delete(actor);
                this._destroyIds?.delete(actor);
            });
            this._destroyIds.set(actor, signalId);
        } catch (_) {
            // A disappearing actor is simply skipped on the next scan.
        }
    }

    _remember(actor) {
        let base = this._actors.get(actor);
        if (!base) {
            const [x, y, z] = actor.get_translation();
            base = {x, y, z};
            this._actors.set(actor, base);
        }
        return base;
    }

    _restore(actor) {
        const base = this._actors.get(actor);
        if (!base)
            return;
        try {
            actor.set_translation(base.x, base.y, base.z);
        } catch (_) {
            // The Shell can destroy overview actors while traversing the stage.
        }
        this._actors.delete(actor);
    }

    _restoreAll() {
        if (!this._actors)
            return;
        for (const actor of [...this._actors.keys()])
            this._restore(actor);
        for (const actor of [...this._compositions])
            this._teardownComposition(actor);
    }

    _teardownComposition(actor) {
        const composition = actor._liquidGlassComposition;
        if (!composition)
            return;
        for (const layer of composition.layers) {
            this._restore(layer);
        }
        composition.overlay.destroy();
        actor.opacity = composition.originalOpacity;
        actor._liquidGlassComposition = null;
        this._compositions.delete(actor);
    }
}
