import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const MAX_OFFSET = 7;
const EFFECT_RADIUS = 2.25;
const MIN_ICON_SIZE = 16;
const MAX_ICON_SIZE = 256;
const ACTOR_SCAN_INTERVAL_US = 250_000;

export default class LiquidGlassParallaxExtension extends Extension {
    enable() {
        this._managedFile = Gio.File.new_for_path(GLib.build_filenamev([
            GLib.get_user_data_dir(),
            'liquid-glass-icon',
            'managed-icons.json',
        ]));
        this._managedIds = new Set();
        this._actors = new Map();
        this._iconActors = new Set();
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
        this._actors = null;
        this._iconActors = null;
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
        for (const actor of this._iconActors) {
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
            currentActors.add(actor);
            const radius = Math.max(width, height) * EFFECT_RADIUS;
            const dx = (pointerX - (x + width / 2)) / radius;
            const dy = (pointerY - (y + height / 2)) / radius;
            const weight = Math.max(0, 1 - Math.hypot(dx, dy));
            if (weight === 0) {
                this._restore(actor);
                continue;
            }
            const base = this._remember(actor);
            actor.set_translation(
                base.x + dx * MAX_OFFSET * weight,
                base.y + dy * MAX_OFFSET * weight,
                base.z,
            );
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
        this._iconActors = new Set(this._managedIconActors());
        this._nextActorScan = now + ACTOR_SCAN_INTERVAL_US;
    }

    *_managedIconActors() {
        const pending = [global.stage];
        const seen = new Set();
        while (pending.length > 0) {
            const actor = pending.pop();
            if (!actor || seen.has(actor))
                continue;
            seen.add(actor);
            const app = actor._delegate?.app ?? actor._delegate?._app;
            const desktopId = app?.get_id?.();
            if (desktopId && this._managedIds.has(desktopId))
                yield actor;
            for (const child of actor.get_children())
                pending.push(child);
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
    }
}
