// Sluice Bandwidth — a GNOME Shell top-bar network throughput indicator.
//
// Self-contained: reads /proc/net/dev directly (~1 Hz), no network access, no root, and no
// dependency on Sluice running. Two display modes — text rates or a compact up/down sparkline.
// Colour convention everywhere: download = green (↓), upload = blue (↑).

import GObject from 'gi://GObject';
import St from 'gi://St';
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import Clutter from 'gi://Clutter';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const DOWN_RGBA = [0.341, 0.890, 0.529, 0.85]; // #57e389 green
const UP_RGBA = [0.384, 0.627, 0.918, 0.85];   // #62a0ea blue
const DOWN_HEX = '#57e389';
const UP_HEX = '#62a0ea';

// A physical NIC has a backing device under /sys; loopback, veth, docker, VPN tunnels (tailscale0,
// wg*, tun*), bridges etc. do not. "Automatic" sums physical NICs only, so VPN traffic (which also
// traverses the physical link) isn't double-counted. The interface picker still lists tunnels so
// they can be watched deliberately; only ephemeral veth pairs are hidden there.
const isPhysical = name => GLib.file_test(`/sys/class/net/${name}/device`, GLib.FileTest.EXISTS);
const isPickable = name => name !== 'lo' && !name.startsWith('veth');

function readProcNetDev() {
    // Returns a Map iface -> {rx, tx} (total bytes), or null on failure.
    try {
        const [ok, bytes] = GLib.file_get_contents('/proc/net/dev');
        if (!ok)
            return null;
        const text = new TextDecoder().decode(bytes);
        const map = new Map();
        for (const line of text.split('\n')) {
            const m = line.match(/^\s*([^:]+):\s*(.*)$/);
            if (!m)
                continue;
            const f = m[2].trim().split(/\s+/);
            map.set(m[1].trim(), {rx: parseInt(f[0], 10) || 0, tx: parseInt(f[8], 10) || 0});
        }
        return map;
    } catch (_e) {
        return null;
    }
}

function listInterfaces() {
    const map = readProcNetDev();
    if (!map)
        return [];
    return [...map.keys()].filter(isPickable).sort();
}

// Sum rx/tx for the chosen interface ("" = all physical NICs).
function readCounters(iface) {
    const map = readProcNetDev();
    if (!map)
        return null;
    let rx = 0, tx = 0;
    for (const [name, c] of map) {
        if (iface ? name !== iface : (name === 'lo' || !isPhysical(name)))
            continue;
        rx += c.rx;
        tx += c.tx;
    }
    return {rx, tx};
}

function fmtRate(bytesPerSec, units) {
    if (units === 'bits') {
        let v = bytesPerSec * 8;
        const u = ['bps', 'Kbps', 'Mbps', 'Gbps'];
        let i = 0;
        while (v >= 1000 && i < u.length - 1) {
            v /= 1000;
            i++;
        }
        return `${i > 0 && v < 10 ? v.toFixed(1) : Math.round(v)} ${u[i]}`;
    }
    let v = bytesPerSec;
    const u = ['B/s', 'KB/s', 'MB/s', 'GB/s'];
    let i = 0;
    while (v >= 1024 && i < u.length - 1) {
        v /= 1024;
        i++;
    }
    return `${i > 0 && v < 10 ? v.toFixed(1) : Math.round(v)} ${u[i]}`;
}

// Compact form for the sparkline's inline labels (no "/s" suffix).
function fmtShort(bytesPerSec, units) {
    const base = units === 'bits' ? 1000 : 1024;
    let v = units === 'bits' ? bytesPerSec * 8 : bytesPerSec;
    const u = ['', 'K', 'M', 'G'];
    let i = 0;
    while (v >= base && i < u.length - 1) {
        v /= base;
        i++;
    }
    return `${i > 0 && v < 10 ? v.toFixed(1) : Math.round(v)}${u[i]}`;
}

const SluiceBwIndicator = GObject.registerClass(
class SluiceBwIndicator extends PanelMenu.Button {
    _init(ext) {
        super._init(0.5, 'Sluice Bandwidth', false);
        this._ext = ext;
        this._settings = ext.getSettings();
        this._history = [];
        this._last = null;
        this._lastTime = GLib.get_monotonic_time();
        this._lastRates = {dn: 0, up: 0};
        this._timer = 0;

        this._box = new St.BoxLayout({style_class: 'panel-status-menu-box sluice-bw-box'});
        this.add_child(this._box);

        this._rebuildIndicator();
        this._buildMenu();

        this._settingsChangedId = this._settings.connect('changed',
            (_s, key) => this._onSettingsChanged(key));

        this._tick();          // seed the baseline
        this._startTimer();
    }

    // ---- top-bar content -------------------------------------------------

    _rebuildIndicator() {
        this._box.destroy_all_children();
        this._label = null;
        this._graph = null;
        this._graphLabel = null;

        if (!this._settings.get_boolean('show-rates')) {
            this._box.add_child(new St.Icon({
                icon_name: 'network-transmit-receive-symbolic',
                style_class: 'system-status-icon',
            }));
            return;
        }

        if (this._settings.get_string('display-mode') === 'graph') {
            this._graph = new St.DrawingArea({
                width: this._settings.get_int('graph-width'),
                y_expand: true,
                y_align: Clutter.ActorAlign.FILL,
                style_class: 'sluice-bw-graph',
            });
            this._graph.connect('repaint', area => this._drawGraph(area));
            this._box.add_child(this._graph);

            this._graphLabel = new St.Label({
                style_class: 'sluice-bw-mini',
                y_align: Clutter.ActorAlign.CENTER,
            });
            this._graphLabel.clutter_text.set_line_wrap(false);
            this._box.add_child(this._graphLabel);
        } else {
            this._label = new St.Label({
                y_align: Clutter.ActorAlign.CENTER,
                style_class: 'sluice-bw-text',
                text: '…',
            });
            this._box.add_child(this._label);
        }
    }

    _drawGraph(area) {
        const cr = area.get_context();
        const [w, h] = area.get_surface_size();
        const mid = h / 2;
        const amp = mid - 1;

        // midline
        cr.setSourceRGBA(1, 1, 1, 0.18);
        cr.setLineWidth(1);
        cr.moveTo(0, mid);
        cr.lineTo(w, mid);
        cr.stroke();

        const hist = this._history;
        const n = hist.length;
        if (n > 1) {
            let max = 1;
            for (const p of hist)
                max = Math.max(max, p.dn, p.up);
            const stepX = w / (n - 1);

            // upload (blue) grows UP from the midline
            cr.setSourceRGBA(...UP_RGBA);
            cr.moveTo(0, mid);
            for (let i = 0; i < n; i++)
                cr.lineTo(i * stepX, mid - (hist[i].up / max) * amp);
            cr.lineTo((n - 1) * stepX, mid);
            cr.closePath();
            cr.fill();

            // download (green) grows DOWN from the midline
            cr.setSourceRGBA(...DOWN_RGBA);
            cr.moveTo(0, mid);
            for (let i = 0; i < n; i++)
                cr.lineTo(i * stepX, mid + (hist[i].dn / max) * amp);
            cr.lineTo((n - 1) * stepX, mid);
            cr.closePath();
            cr.fill();
        }
        cr.$dispose();
    }

    // ---- menu ------------------------------------------------------------

    _buildMenu() {
        this._rateItem = new PopupMenu.PopupMenuItem('↓ …    ↑ …', {reactive: false});
        this.menu.addMenuItem(this._rateItem);
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        this._showSwitch = new PopupMenu.PopupSwitchMenuItem(
            'Show in top bar', this._settings.get_boolean('show-rates'));
        this._showSwitch.connect('toggled',
            (_i, s) => this._settings.set_boolean('show-rates', s));
        this.menu.addMenuItem(this._showSwitch);

        this._displaySub = new PopupMenu.PopupSubMenuMenuItem('Display');
        this._addRadio(this._displaySub, 'display-mode',
            [['text', 'Text (rates)'], ['graph', 'Graph (sparkline)']]);
        this.menu.addMenuItem(this._displaySub);

        this._unitsSub = new PopupMenu.PopupSubMenuMenuItem('Units');
        this._addRadio(this._unitsSub, 'units',
            [['bytes', 'Bytes/sec (KB/s, MB/s)'], ['bits', 'Bits/sec (Mbps)']]);
        this.menu.addMenuItem(this._unitsSub);

        this._ifaceSub = new PopupMenu.PopupSubMenuMenuItem('Interface');
        this.menu.addMenuItem(this._ifaceSub);
        this._rebuildIfaceMenu();

        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._addAction('Settings…', () => this._ext.openPreferences());
        this._addAction('Open Sluice', () => this._spawn(['sluice-ui']));
        this._addAction('Quit Sluice', () => this._spawn(['pkill', '-x', 'sluice-ui']));
    }

    _addAction(label, fn) {
        const it = new PopupMenu.PopupMenuItem(label);
        it.connect('activate', fn);
        this.menu.addMenuItem(it);
        return it;
    }

    _addRadio(sub, key, opts) {
        sub._sluiceKey = key;
        sub._sluiceItems = [];
        for (const [val, lbl] of opts) {
            const it = new PopupMenu.PopupMenuItem(lbl);
            it._sluiceVal = val;
            it.connect('activate', () => this._settings.set_string(key, val));
            sub.menu.addMenuItem(it);
            sub._sluiceItems.push(it);
        }
        this._refreshRadio(sub);
    }

    _refreshRadio(sub) {
        const cur = this._settings.get_string(sub._sluiceKey);
        for (const it of sub._sluiceItems) {
            it.setOrnament(it._sluiceVal === cur
                ? PopupMenu.Ornament.DOT : PopupMenu.Ornament.NONE);
        }
    }

    _rebuildIfaceMenu() {
        this._ifaceSub.menu.removeAll();
        this._ifaceSub._sluiceKey = 'interface';
        this._ifaceSub._sluiceItems = [];
        const add = (val, lbl) => {
            const it = new PopupMenu.PopupMenuItem(lbl);
            it._sluiceVal = val;
            it.connect('activate', () => this._settings.set_string('interface', val));
            this._ifaceSub.menu.addMenuItem(it);
            this._ifaceSub._sluiceItems.push(it);
        };
        add('', 'Automatic (physical)');
        for (const n of listInterfaces())
            add(n, n);
        this._refreshRadio(this._ifaceSub);
    }

    _spawn(argv) {
        try {
            Gio.Subprocess.new(argv, Gio.SubprocessFlags.NONE);
        } catch (e) {
            logError(e, 'sluice-bandwidth: spawn failed');
        }
    }

    // ---- sampling --------------------------------------------------------

    _tick() {
        const c = readCounters(this._settings.get_string('interface'));
        const now = GLib.get_monotonic_time();
        if (c && this._last) {
            const dt = (now - this._lastTime) / 1e6;
            if (dt > 0) {
                const dn = Math.max(0, (c.rx - this._last.rx) / dt);
                const up = Math.max(0, (c.tx - this._last.tx) / dt);
                this._lastRates = {dn, up};
                this._history.push({dn, up});
                const cap = Math.max(8, this._settings.get_int('graph-width'));
                while (this._history.length > cap)
                    this._history.shift();
                this._updateDisplay();
            }
        }
        this._last = c;
        this._lastTime = now;
        return GLib.SOURCE_CONTINUE;
    }

    _updateDisplay() {
        const units = this._settings.get_string('units');
        const {dn, up} = this._lastRates;
        const showDn = this._settings.get_boolean('show-down');
        const showUp = this._settings.get_boolean('show-up');

        if (this._rateItem)
            this._rateItem.label.text = `↓ ${fmtRate(dn, units)}     ↑ ${fmtRate(up, units)}`;

        if (this._label) {
            const parts = [];
            if (showDn)
                parts.push(`↓ ${fmtRate(dn, units)}`);
            if (showUp)
                parts.push(`↑ ${fmtRate(up, units)}`);
            this._label.text = parts.join('   ') || '—';
        }

        if (this._graphLabel) {
            const lines = [];
            if (showUp)
                lines.push(`<span color="${UP_HEX}">↑ ${fmtShort(up, units)}</span>`);
            if (showDn)
                lines.push(`<span color="${DOWN_HEX}">↓ ${fmtShort(dn, units)}</span>`);
            this._graphLabel.clutter_text.set_markup(lines.join('\n') || ' ');
        }

        if (this._graph)
            this._graph.queue_repaint();
    }

    // ---- timer + settings ------------------------------------------------

    _startTimer() {
        this._removeTimer();
        this._timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT,
            this._settings.get_int('refresh-interval'), () => this._tick());
    }

    _removeTimer() {
        if (this._timer) {
            GLib.Source.remove(this._timer);
            this._timer = 0;
        }
    }

    _onSettingsChanged(key) {
        switch (key) {
        case 'show-rates':
            this._showSwitch.setToggleState(this._settings.get_boolean('show-rates'));
            this._rebuildIndicator();
            this._updateDisplay();
            break;
        case 'display-mode':
            this._refreshRadio(this._displaySub);
            this._rebuildIndicator();
            this._updateDisplay();
            break;
        case 'units':
            this._refreshRadio(this._unitsSub);
            this._updateDisplay();
            break;
        case 'interface':
            this._refreshRadio(this._ifaceSub);
            this._last = null;          // reset the baseline for the new interface
            this._history = [];
            break;
        case 'refresh-interval':
            this._startTimer();
            break;
        case 'graph-width':
            this._rebuildIndicator();
            break;
        case 'show-down':
        case 'show-up':
            this._updateDisplay();
            break;
        }
    }

    destroy() {
        this._removeTimer();
        if (this._settingsChangedId) {
            this._settings.disconnect(this._settingsChangedId);
            this._settingsChangedId = 0;
        }
        super.destroy();
    }
});

export default class SluiceBandwidthExtension extends Extension {
    enable() {
        this._indicator = new SluiceBwIndicator(this);
        Main.panel.addToStatusArea(this.uuid, this._indicator, 0, 'right');
    }

    disable() {
        this._indicator?.destroy();
        this._indicator = null;
    }
}
