// Sluice Bandwidth — preferences (libadwaita).

import Adw from 'gi://Adw';
import Gtk from 'gi://Gtk';
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';

import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

// The picker lists non-loopback interfaces (VPN tunnels included, so they can be watched
// deliberately); only ephemeral veth pairs are hidden. "Automatic" in the indicator sums physical
// NICs only — see extension.js.
function listInterfaces() {
    try {
        const [ok, bytes] = GLib.file_get_contents('/proc/net/dev');
        if (!ok)
            return [];
        const out = [];
        for (const line of new TextDecoder().decode(bytes).split('\n')) {
            const m = line.match(/^\s*([^:]+):/);
            if (!m)
                continue;
            const n = m[1].trim();
            if (n !== 'lo' && !n.startsWith('veth'))
                out.push(n);
        }
        return out.sort();
    } catch (_e) {
        return [];
    }
}

export default class SluiceBandwidthPrefs extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();
        const page = new Adw.PreferencesPage();
        window.add(page);

        // ---- Display -----------------------------------------------------
        const disp = new Adw.PreferencesGroup({title: 'Display'});
        page.add(disp);

        const showRow = new Adw.SwitchRow({title: 'Show in the top bar'});
        settings.bind('show-rates', showRow, 'active', Gio.SettingsBindFlags.DEFAULT);
        disp.add(showRow);

        const modeRow = new Adw.ComboRow({
            title: 'Mode',
            subtitle: 'Text rates, or a compact up/down sparkline',
            model: new Gtk.StringList({strings: ['Text (rates)', 'Graph (sparkline)']}),
        });
        modeRow.selected = settings.get_string('display-mode') === 'graph' ? 1 : 0;
        modeRow.connect('notify::selected',
            r => settings.set_string('display-mode', r.selected === 1 ? 'graph' : 'text'));
        disp.add(modeRow);

        const unitsRow = new Adw.ComboRow({
            title: 'Units',
            model: new Gtk.StringList({strings: ['Bytes/sec (KB/s, MB/s)', 'Bits/sec (Mbps)']}),
        });
        unitsRow.selected = settings.get_string('units') === 'bits' ? 1 : 0;
        unitsRow.connect('notify::selected',
            r => settings.set_string('units', r.selected === 1 ? 'bits' : 'bytes'));
        disp.add(unitsRow);

        const dnRow = new Adw.SwitchRow({title: 'Show download (↓, green)'});
        settings.bind('show-down', dnRow, 'active', Gio.SettingsBindFlags.DEFAULT);
        disp.add(dnRow);

        const upRow = new Adw.SwitchRow({title: 'Show upload (↑, blue)'});
        settings.bind('show-up', upRow, 'active', Gio.SettingsBindFlags.DEFAULT);
        disp.add(upRow);

        const gwRow = new Adw.SpinRow({
            title: 'Graph width',
            subtitle: 'Pixels (graph mode only)',
            adjustment: new Gtk.Adjustment({
                lower: 40, upper: 200, step_increment: 2, page_increment: 10,
                value: settings.get_int('graph-width'),
            }),
        });
        gwRow.connect('notify::value', r => settings.set_int('graph-width', Math.round(r.value)));
        disp.add(gwRow);

        // ---- Source ------------------------------------------------------
        const src = new Adw.PreferencesGroup({title: 'Source'});
        page.add(src);

        const ifaces = listInterfaces();
        const ifRow = new Adw.ComboRow({
            title: 'Network interface',
            subtitle: 'Automatic combines all physical NICs (VPN tunnels excluded to avoid double-counting)',
            model: new Gtk.StringList({strings: ['Automatic (physical)', ...ifaces]}),
        });
        const cur = settings.get_string('interface');
        ifRow.selected = cur ? Math.max(0, ifaces.indexOf(cur) + 1) : 0;
        ifRow.connect('notify::selected',
            r => settings.set_string('interface', r.selected === 0 ? '' : (ifaces[r.selected - 1] || '')));
        src.add(ifRow);

        const intRow = new Adw.SpinRow({
            title: 'Refresh interval',
            subtitle: 'Milliseconds',
            adjustment: new Gtk.Adjustment({
                lower: 500, upper: 5000, step_increment: 250, page_increment: 500,
                value: settings.get_int('refresh-interval'),
            }),
        });
        intRow.connect('notify::value', r => settings.set_int('refresh-interval', Math.round(r.value)));
        src.add(intRow);
    }
}
