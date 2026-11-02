# samsung-5g-nsa-fix

Make unmodified apps (4Cells, NetMonster-class) display the real NR ARFCN
(and PCI) for 5G NSA cells on a Samsung Snapdragon phone
(**SM-S936B / S25+, S936BXXUCDZIF**) where the stock telephony stack leaves
them empty.

## The one-paragraph mechanism

Nothing in any app is rewritten — and `getNrarfcn()` itself is never
replaced. The method just returns a field (`mNrarfcn`) of a
`CellIdentityNr` object that the app receives **already constructed**
through binder. That object is built in one place: the
`com.android.phone` system process, which receives the vendor RIL's
report (where Samsung drops the NR ARFCN for NSA cells and fills a
placeholder), and then parcels the object out to every app. The patch
sits at that single factory: a Frida hook (injected as root into
`com.android.phone`) lets the constructor finish, and **if the field is a
placeholder, overwrites it with the real value** read from the modem's own
DIAG diagnostic stream before the object is parceled. Every unmodified app
downstream then deserializes an object that already contains the truth, and
its plain `getNrarfcn()` call returns it. Fixing the producer fixes every
consumer at once; fixing it in the app's process would be impossible
anyway, because by then the real value never crossed the parcel boundary.

Full component map, data flow, failure modes and open items:
[DESIGN.md](DESIGN.md). Protocol and recon details: [NOTES.md](NOTES.md).

## Components

| Piece | What it is |
|---|---|
| `nsa5gfix-diagd.rs` | Static aarch64 Rust daemon. Runs as root, reads the Qualcomm DIAG stream (`/dev/diag` diagchar memory-device mode by default; `smdcntl8`/`ffs` fallbacks), enables the NR RRC Serving Cell Info / NR ML1 / LTE serving logs, publishes `{ARFCN, PCI, band, age}` to `dev.nsa5gfix.*` system properties. Self-tests its parser. |
| `nsa5gfix-hook.js` | Frida script injected into `com.android.phone`. Substitutes placeholder `mNrarfcn`/`mPci`/`mBands` on `CellIdentityNr` construction when the DIAG cache is fresh. Never overwrites real values, never fabricates entries, fully guarded. |
| `run.sh` | Host-driven session driver: recorded root chain → channel recon → daemon → frida-server → hook attach. |
| `app/` → `NSA5GFix.apk` | Notification-only wrapper app (nsg-unlock app style) hosting the daemon as its root child; the tray card shows the live NR state and Stop kills it. |

Everything is volatile: a reboot (or restarting `com.android.phone`)
returns the system to stock. Nothing is flashed or persisted.

## Status

Built and unit-verified host-side; **not yet validated on a live NSA
session** (needs the phone on adb, a fresh boot for the one-attempt root
chain, and EN-DC coverage). See "Open items" in DESIGN.md — including the
known possibility that Samsung emits *no* `CellInfoNr` at all in NSA, in
which case the fix extends to appending a synthetic serving-cell entry at
the same producer point (still no app modification).
