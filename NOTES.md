# samsung-5g-nsa-fix — NSA NR ARFCN via runtime patch (SM-S936B / S936BXXUCDZIF)

Goal: unmodified apps (4Cells etc.) receive a real NR ARFCN (and PCI when
absent) through the stock `CellIdentityNr.getNrarfcn()` path while in 5G NSA.

Architecture: root daemon `nsa5gfix-diagd` reads the Qualcomm DIAG stream locally
(same data Network Signal Guru reads with root), extracts the NR serving
cell {ARFCN, PCI, band} and publishes it to system properties; a Frida hook
inside `com.android.phone` substitutes those values into `CellIdentityNr`
objects the vendor RIL built with placeholders. Everything downstream
(TelephonyRegistry parcels, app-side `getNrarfcn()`) flows stock.

## Verified facts

- Phone: SM-S936B (pa2q), S936BXXUCDZIF, kernel 6.6.127-android15-8,
  Snapdragon 8 Elite. Temp root: device-tested 2026-10-15 payload
  (`Root-My-Galaxy-Payloads` branch S936BXXUCDZIF,
  `tools/root-pa2q-S936BXXUCDZIF.sh`). Volatile per boot.
- The vendor RIL reports the LTE anchor's EARFCN but no NR ARFCN for the
  NSA NR cell (RILJ logcat evidence 2026-10-29). `PhysicalChannelConfig`
  channel numbers are 0 even for LTE on this build.
- DIAG module loaded (`/sys/module/diag`), `/dev/ffs-diag{,-1,-2}` and
  `/dev/at_mdm0`, `/dev/smd*` exist; `/dev/diag` not visible to the SHELL
  context (ENOENT). NSG reportedly reads SSB-ARFCN with root on this phone,
  so a root-accessible local channel exists — identify it during recon
  (candidates: /dev/diag as root, /dev/smdcntl8, ffs-diag endpoints).

## DIAG protocol (from QCSuper + SCAT references in refs/)

- Framing: payload = cmd(1B) + body + crc_le(2B); escape 0x7d→7d 5d,
  0x7e→7d 5e; trailer 0x7e. CRC-16/X.25: init 0xFFFF, poly 0x8408
  (reflected 0x1021), xorout 0xFFFF, appended little-endian.
- /dev/diag (diagchar): ioctl 7 SWITCH_LOGGING MEMORY_DEVICE_MODE=2
  (int, fallback struct {int,int,int}); ioctl 32 REMOTE_DEV → use_mdm.
  Write: [u32 32][(u32 -1) if use_mdm] + framed payload.
  Read:  [u32 32][(u32)] [i32 nb] then nb × [i32 size][payload].
- DIAG_LOG_F = 16 (0x10): log header `<BBHHHQ` = cmd,rsv,len1,len2,log_id,
  ts(8B) — 16 bytes, body follows; len1 = body_len + 4.
- DIAG_LOG_CONFIG_F = 115 (0x73): retrieve ranges: pack('<3xI', 1),
  response `<3xII` + 16×u32 mask bitsizes per log type.
  set mask: pack('<3xIII', 3, type, bitsize) + mask bytes
  (bit i enables log code (type<<12)|i).
- Log codes (type 0xB): 0xB0C2 LTE RRC Serving Cell, 0xB821 NR RRC OTA,
  0xB823 NR RRC Serving Cell Info, 0xB97F NR ML1 Meas DB Update.
- NR RRC Serving Cell Info body (after 16B header): version `<HH`
  (rel_min, rel_maj): 0.4: `<H LLHH Q H BH B LH` = pci, dl_arfcn, ul_arfcn,
  dlbw, ulbw, cellid, mcc, mnc_digit, mnc, allowed_access, tac, band;
  3.0: pci, nr_cgi(8B), dl_arfcn...; 3.2/3.3: +3B skip;
  3.4: 4-byte prefix then as 3.0.

## 4Cells 2.3.0 data-acquisition paths (from the pinned APK, verified in smali)

Analyzed via the owner's prior signal-buddy probe (`4Cells_2.3.0.apk`,
SHA-256 `1bc49364…4339`, full apktool decode re-done for this project):

1. **Primary — plain getters.** `getAllCellInfo()` → `CellInfoNr` →
   `CellIdentityNr.getNci()/getTac()/getPci()/getNrarfcn()`
   (`o/qy`, bridges in `o/my`). Validity gates: PCI accepted only when
   `0 <= pci < 2001`; invalid values leave the export field empty.
   NR records nest as `ChildData` under the LTE root record.
2. **Fallback — toString() regex parse** (`o/vy1` + `o/ad2`): patterns
   `mNrArfcn|nr_arfcn|nrarfcn|nrArfcn\s*=\s*(-?\d+)`, `mPci\s*=\s*(-?\d+)`,
   `mTac`, `mNci`, `mBands[...]` applied to the cell object's toString()
   inside try/catch (used when getter paths fail/throw).
3. **No other sources.** No `PhysicalChannelConfig`, no ServiceState PCI,
   no NSG/DIAG imports in the NR path.

Consequence for the patch: both paths consume the same `mPci`/`mNrarfcn`
fields of the same `CellIdentityNr` object, so the field substitution in
com.android.phone covers both — the getters return the substituted values
and `toString()` prints them. Hook refinement from this analysis: PCI 0 is
a valid PCI, so substitution fires only on UNAVAILABLE/negative markers.

## The notification-only wrapper app (`app/`, nsg-unlock app style)

`NSA5GFix.apk` — UI-less trampoline (`LaunchActivity`) → `DaemonHost` (single
owner of the root child: stages the bundled `jniLibs/arm64-v8a/libnsa5gfix-diagd.so`
to `/data/local/tmp/nsa5gfix-diagd` and execs it via `su`, output into a bounded
ring) → `DaemonService` (specialUse FGS whose card IS the app): the tray
shows the live `dev.nsa5gfix.*` state (ARFCN · nBand · PCI · age) polled
every 1 s, with a Stop action that root-kills the daemon by the pid it
published (`dev.nsa5gfix.pid`) and stops itself when the child is gone.

Build gates (`app/build.sh`): host selftest of the parser, aarch64 static
rebuild, and a byte-identity check of the APK-packaged daemon against the
fresh binary. AGP 9 built-in Kotlin (no kotlin-android plugin);
`repo.maven.apache.org` is 403-blocked on this network — the Google Maven
Central mirror is wired first in `settings.gradle.kts`.

The app is daemon-only: the Frida attach into com.android.phone stays
host-driven (`run.sh`). On first launch the KernelSU consent prompt
must be accepted (the su call blocks until then).

## Open items

- Channel recon on device (as root) before first daemon run. Must also
  decide NSG coexistence: identify Network Signal Guru's own channel
  (run it capturing, then inspect its open fds + `getprop sys.usb.config`
  as root). If NSG is a diagchar memory-device client too, both coexist
  (per-client masks/streams by design). If NSG holds the USB/DM diag
  composition (ffs-diag) the stream is single-consumer and the modes are
  mutually exclusive — then time-share, or relax the hook freshness TTL
  so 4Cells keeps the last value across an NSG session.
- If Samsung emits NO CellInfoNr at all during NSA (4Cells shows no PCI
  either — suggests this), substitution alone never fires: extend the hook
  to append a synthetic registered CellInfoNr to the getAllCellInfo/
  cell-info-list response (built via reflection) when NSA + fresh cache.
- Idle-NSA (no SCG): NR info only via ML1 raster ARFCN — publish as a
  separate prop; substitution still gated on CellIdentityNr existing.
- SSB vs serving ARFCN: use 0xB823 dl_nrarfcn (serving carrier) as the
  nrarfcn; matches what SA devices report through the same API.
- Full validation needs a live EN-DC session (currently LTE-only at the
  usual phone location; check `dumpsys telephony.registry` for
  isEnDcAvailable=true when testing).
