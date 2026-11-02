// nsa5gfix-hook.js — fill NSA NR ARFCN/PCI into CellIdentityNr inside com.android.phone.
// Values come from the nsa5gfix-diagd root daemon via system properties
// (dev.nsa5gfix.*). Substitution only: placeholder values are replaced when
// a fresh DIAG-derived value exists; real values are never overwritten and
// nothing is fabricated. Every step guarded; on any error acts transparent.

'use strict';

const PROP_PREFIX = 'dev.nsa5gfix.';
const TTL_MS = 20000;

function log(msg) { console.log('[nsa5gfix] ' + msg); }

function readCache() {
    try {
        const SP = Java.use('android.os.SystemProperties');
        const now = Java.use('android.os.SystemClock').elapsedRealtime();
        const ts = parseInt(SP.get(PROP_PREFIX + 'ts', '0'), 10);
        if (!isFinite(ts) || ts <= 0 || now < ts || now - ts > TTL_MS) return null;
        const arfcn = parseInt(SP.get(PROP_PREFIX + 'arfcn', '-1'), 10);
        const pci = parseInt(SP.get(PROP_PREFIX + 'pci', '-1'), 10);
        const band = parseInt(SP.get(PROP_PREFIX + 'band', '-1'), 10);
        if (!isFinite(arfcn) || arfcn <= 0 || arfcn > 3279165) return null;
        return {
            arfcn: arfcn,
            pci: isFinite(pci) ? pci : -1,
            band: isFinite(band) ? band : -1,
        };
    } catch (e) {
        return null;
    }
}

function fieldNamed(instance, re) {
    // Locate a declared field case-insensitively by regex; returns a
    // JS accessor pair or null. AOSP field names moved across versions,
    // so resolve them defensively instead of hardcoding.
    try {
        const cls = instance.getClass();
        const fields = cls.getDeclaredFields();
        for (let i = 0; i < fields.length; i++) {
            const name = fields[i].getName();
            if (re.test(name)) {
                const fn = name;
                return {
                    get: function () { return instance[fn].value; },
                    set: function (v) { instance[fn].value = v; },
                };
            }
        }
    } catch (e) { log('field scan failed: ' + e); }
    return null;
}

const UNAVAILABLE = 0x7fffffff;

function fixInstance(instance) {
    try {
        const cache = readCache();
        if (cache === null) return;
        const fArfcn = fieldNamed(instance, /^m?nrarfcn$/i);
        if (fArfcn !== null) {
            const cur = fArfcn.get();
            if (cur === UNAVAILABLE || cur === 0) {
                fArfcn.set(cache.arfcn);
                log('nrarfcn ' + cur + ' -> ' + cache.arfcn +
                    ' (pci cache ' + cache.pci + ')');
            }
        }
        if (cache.pci >= 0 && cache.pci <= 1007) {
            const fPci = fieldNamed(instance, /^m?pci$/i);
            if (fPci !== null) {
                const cur = fPci.get();
                // PCI 0 is a VALID PCI (range 0..1007): substitute only on
                // the explicit unavailable markers, never on 0.
                if (cur === UNAVAILABLE || cur < 0) {
                    fPci.set(cache.pci);
                    log('pci ' + cur + ' -> ' + cache.pci);
                }
            }
        }
        if (cache.band > 0) {
            const fBands = fieldNamed(instance, /^m?bands$/i);
            if (fBands !== null) {
                try {
                    const bands = fBands.get();
                    if (bands === null || bands.length === 0) {
                        fBands.set(Java.array('int', [cache.band]));
                        log('bands <- [' + cache.band + ']');
                    }
                } catch (e) { /* bands optional */ }
            }
        }
    } catch (e) {
        log('fix failed (transparent): ' + e);
    }
}

Java.perform(function () {
    let CellIdentityNr;
    try {
        CellIdentityNr = Java.use('android.telephony.CellIdentityNr');
    } catch (e) {
        log('CellIdentityNr unavailable in this process: ' + e);
        return;
    }

    const ctors = CellIdentityNr.$init.overloads;
    ctors.forEach(function (m) {
        m.implementation = function () {
            const r = m.apply(this, arguments);
            fixInstance(this);
            return r;
        };
    });
    log('hooked CellIdentityNr (' + ctors.length + ' ctors) in ' +
        Java.use('android.app.ActivityThread').currentProcessName());
});
