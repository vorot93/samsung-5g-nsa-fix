# Architecture

Why unmodified apps start showing the real NSA NR ARFCN — and where each
piece runs, what it may and may not do, and what is still unproven.

## 1. Where the value is lost, and why the fix sits where it sits

The Android cell-info pipeline on this device:

```
MODEM (knows the real NR ARFCN for the NSA NR cell)
  │
  │  vendor RIL (Samsung) — reports the LTE anchor's EARFCN, but for the
  │  NSA NR secondary cell hands the framework no NR ARFCN (placeholder
  │  MAX_INT/0; measured on-device in RILJ logcat, 2026-10-29)
  ▼
com.android.phone  (system telephony process)         ← THE PATCH POINT
  │  RILJ converts the HAL response and CONSTRUCTS android.telephony.
  │  CellIdentityNr here — one factory, one process, all consumers.
  │
  │  [nsa5gfix-hook.js] hooked into this process: right after any CellIdentityNr
  │  constructor runs, if a field is a placeholder and the DIAG cache is
  │  fresh (<20 s), overwrite it with the real value. Real values are
  │  never touched; nothing is fabricated.
  │
  │  [nsa5gfix-diagd] feeds the cache: root daemon reading the modem's own DIAG
  │  stream → dev.nsa5gfix.{arfcn,pci,band,ts} properties (the same data
  │  Network Signal Guru displays with root).
  ▼  binder parcel — the object's field values travel as data
system_server (TelephonyRegistry) — forwards copies, unaware
  ▼  binder parcel
APP PROCESS, e.g. 4Cells — UNMODIFIED
  │  CellIdentityNr deserialized with the real ARFCN already inside
  ▼
  CellIdentityNr.getNrarfcn()  →  returns the field  →  real value
```

Three consequences worth stating explicitly:

- **`getNrarfcn()` is never replaced, in any process.** The app calls the
  stock method on a stock object; the object's field was corrected one
  process upstream, before the value crossed the parcel boundary into app
  space. 4Cells' APK, code and permissions are untouched.
- **Fixing it in the app's process would not work even if we wanted to.**
  By the time 4Cells holds the object, the real value is already gone from
  its data — the parcel contains the placeholder. Only the producer can
  re-source the value.
- **One hook covers every consumer.** 4Cells reads via
  `getPci()/getNrarfcn()` and additionally regex-parses `toString()`
  (verified in its smali); both read the same fields of the same object,
  and `toString()` prints the substituted field too.

## 2. Components and contracts

### `nsa5gfix-diagd` (Rust, static aarch64 musl, root)

- **Input**: the Qualcomm DIAG protocol. Default channel `/dev/diag`
  (diagchar: `SWITCH_LOGGING`→`MEMORY_DEVICE_MODE`, `REMOTE_DEV`, framed
  HDLC read/write); fallbacks `/dev/smdcntl8` and functionfs `ffs-diag`
  endpoints. Channel choice is settled by on-device recon (see NOTES.md,
  including the NSG-coexistence question).
- **Registers interest** via `DIAG_LOG_CONFIG` for log type 0xB items
  `0xB823` (NR RRC Serving Cell Info: PCI, DL NR-ARFCN, band, TAC, CID),
  `0xB97F` (NR ML1 Meas DB: SSB raster ARFCN fallback) and `0xB0C2`
  (LTE anchor, context only).
- **Output**: system properties — `dev.nsa5gfix.arfcn`, `.pci`, `.band`,
  `.ts` (CLOCK_BOOTTIME ms, comparable to `elapsedRealtime()` in any
  process), `.pid` (for the app's verified Stop), `.ssb_raster`. Written
  on change only, via `setprop` as root.
- **Self-gate**: `selftest` round-trips a synthetic 0xB823 v3.4 frame
  through framing + parsing; `app/build.sh` refuses to ship without it.

### `nsa5gfix-hook.js` (Frida, inside `com.android.phone`)

- Hooks **every** `CellIdentityNr.$init` overload (constructor), so it is
  independent of Android-version signature drift and catches Samsung's
  internal construction paths too.
- After the original constructor runs: field-scan by name (defensive),
  substitute `mNrarfcn` when `MAX_INT/0`, `mPci` when
  `MAX_INT`/negative (**0 is a valid PCI — never treated as a
  placeholder**), `mBands` when empty.
- Freshness gate: only substitutes when `dev.nsa5gfix.ts` is <20 s old;
  stale cache → fully transparent. Every step try/catch-guarded.
- Loaded per-boot by attaching frida (host) to `com.android.phone`.

### `app/` → `NSA5GFix.apk` (notification-only wrapper, nsg-unlock style)

- `LaunchActivity`: UI-less trampoline; notification permission is
  load-bearing (the card is the only Stop handle).
- `DaemonHost`: single owner of the root child — stages the bundled
  `libnsa5gfix-diagd.so` to `/data/local/tmp/nsa5gfix-diagd`, execs via `su` (KernelSU
  consent on first run), ring-buffers output; Stop is a verified
  root-side kill of the pid the daemon published.
- `DaemonService`: specialUse FGS; the card shows the live
  `dev.nsa5gfix.*` state (polled every 1 s) — the on-device payoff display.
  Stops itself when the child dies; the card never outlives the session.

### `run.sh` (host driver)

Recorded root chain (Root-My-Galaxy-Payloads `pa2q-S936BXXUCDZIF`,
one attempt per fresh boot, ~10 min) → push components → DIAG channel
recon (incl. NSG's channel) → daemon → frida-server → attach hook.

## 3. Lifetime and failure posture

- **Everything is volatile.** Root, daemon, hook, cache die on reboot;
  restarting `com.android.phone` clears the hook alone. Nothing flashed,
  no persistent state except the installed APK (which alone does nothing
  without the root chain).
- **Fail-transparent by construction**: no fresh DIAG value → no
  substitution; hook errors → object built as stock; daemon death →
  values age out in 20 s → apps see stock behavior again.
- **Risk surface**: injecting into `com.android.phone` can destabilize
  telephony if the hook were to throw — hence total guarding and
  substitution-only semantics (no list mutation in the current build).

## 4. Open items

1. **The synthetic-entry extension (design ready, not built).** If live
   EN-DC shows Samsung emits *no* `CellInfoNr` at all in NSA (4Cells
   showing no PCI suggests this), substitution has nothing to fire on.
   Then the same hook extends to the cell-info *list* boundary in
   `com.android.phone` (getAllCellInfo/unsolicited list): append a
   synthetic registered `CellInfoNr` built via reflection from the DIAG
   cache when NSA is active — still at the producer, still no app
   modification.
2. On-device channel recon (root): confirm `/dev/diag` visibility, pick
   channel, and identify NSG's channel for coexistence (diagchar
   memory-mode clients coexist by design; USB/DM stream is
   single-consumer — details in NOTES.md).
3. Live validation under EN-DC with 4Cells (phone on adb, fresh boot,
   5G NSA coverage).
4. SSB vs serving ARFCN semantics: currently publishes 0xB823's
   `dl_nrarfcn` (serving carrier), matching what SA devices report
   through the same API; the ML1 raster ARFCN is kept as a separate
   property.
