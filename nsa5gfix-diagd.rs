// nsa5gfix-diagd — DIAG-based NSA NR serving-cell publisher for the samsung-5g-nsa-fix patch.
//
// Reads the Qualcomm DIAG stream on-device (as root), enables the NR RRC
// Serving Cell Info (0xB823), NR ML1 Meas DB Update (0xB97F) and LTE RRC
// Serving Cell (0xB0C2) logs, extracts the NR serving cell
// {dl NR-ARFCN, PCI, band, TAC, cell id} and publishes them to system
// properties (dev.nsa5gfix.*) for the Frida hook inside com.android.phone.
//
// Protocol facts (QCSuper + SCAT references, see NOTES.md):
//   HDLC-lite framing: escaped(cmd+body+crc16_le) + 0x7e
//   CRC-16/X.25: init 0xffff, poly 0x8408 reflected, xorout 0xffff, LE
//   DIAG_LOG_F = 16; DIAG_LOG_CONFIG_F = 115; log header <BBHHHQ (16B)
//   /dev/diag (diagchar): ioctl 7 SWITCH_LOGGING(2), ioctl 32 REMOTE_DEV;
//   write [u32 32][(u32 -1) if mdm][frame]; read [u32 32][(u32)][i32 n]{[i32 sz][payload]}
//
// Build (static, runs on Android):
//   rustc -O --target aarch64-unknown-linux-musl nsa5gfix-diagd.rs -o nsa5gfix-diagd
// Selftest (host): ./nsa5gfix-diagd selftest

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::process::Command;
use std::time::Duration;

const DIAG_LOG_F: u8 = 16;
const DIAG_LOG_CONFIG_F: u8 = 115;
const LOG_CONFIG_RETRIEVE_ID_RANGES_OP: u32 = 1;
const LOG_CONFIG_SET_MASK_OP: u32 = 3;

const LOG_LTE_RRC_SERVING_CELL: u16 = 0xb0c2;
const LOG_NR_RRC_SERVING_CELL: u16 = 0xb823;
const LOG_NR_ML1_MEAS_DB: u16 = 0xb97f;

const PROP_PREFIX: &str = "dev.nsa5gfix.";
const MAX_ARFCN: u32 = 3_300_000;

extern "C" {
    fn ioctl(fd: i32, request: std::os::raw::c_ulong, ...) -> i32;
    fn clock_gettime(clk_id: std::os::raw::c_int, tp: *mut Timespec) -> i32;
}

#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

const CLOCK_BOOTTIME: std::os::raw::c_int = 7;
const DIAG_IOCTL_SWITCH_LOGGING: std::os::raw::c_ulong = 7;
const DIAG_IOCTL_REMOTE_DEV: std::os::raw::c_ulong = 32;
const MEMORY_DEVICE_MODE: i32 = 2;
const USER_SPACE_DATA_TYPE: u32 = 32;

fn boottime_ms() -> i64 {
    unsafe {
        let mut ts = Timespec { tv_sec: 0, tv_nsec: 0 };
        if clock_gettime(CLOCK_BOOTTIME, &mut ts) == 0 {
            ts.tv_sec * 1000 + ts.tv_nsec / 1_000_000
        } else {
            0
        }
    }
}

// ---------- CRC-16/X.25 ----------

fn crc16_x25(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &b in data {
        crc = (crc >> 8) ^ table_byte((crc ^ b as u16) & 0xff);
    }
    crc ^ 0xffff
}

fn table_byte(idx: u16) -> u16 {
    // Reflected 0x1021 (0x8408) table computed on demand.
    const POLY: u16 = 0x8408;
    let mut v = idx;
    for _ in 0..8 {
        v = if v & 1 != 0 { (v >> 1) ^ POLY } else { v >> 1 };
    }
    v
}

// ---------- HDLC framing ----------

fn escape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 8);
    for &b in data {
        match b {
            0x7d => out.extend_from_slice(&[0x7d, 0x5d]),
            0x7e => out.extend_from_slice(&[0x7d, 0x5e]),
            _ => out.push(b),
        }
    }
    out
}

fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0x7d && i + 1 < data.len() {
            out.push(data[i + 1] ^ 0x20);
            i += 2;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

fn frame(cmd: u8, body: &[u8]) -> Vec<u8> {
    let mut inner = Vec::with_capacity(body.len() + 3);
    inner.push(cmd);
    inner.extend_from_slice(body);
    let crc = crc16_x25(&inner);
    inner.extend_from_slice(&crc.to_le_bytes());
    let mut framed = escape(&inner);
    framed.push(0x7e);
    framed
}

// Splits a raw stream into unescaped frames (contents, CRC verified+stripped).
fn split_frames(buf: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for i in 0..buf.len() {
        if buf[i] == 0x7e {
            if i > start {
                let inner = unescape(&buf[start..i]);
                if inner.len() >= 3 {
                    let (payload, crc_bytes) = inner.split_at(inner.len() - 2);
                    let got = u16::from(crc_bytes[0]) | (u16::from(crc_bytes[1]) << 8);
                    if crc16_x25(payload) == got {
                        out.push(payload.to_vec());
                    }
                }
            }
            start = i + 1;
        }
    }
    out
}

// ---------- transports ----------

trait Transport {
    fn send(&mut self, cmd: u8, body: &[u8]) -> std::io::Result<()>;
    fn recv(&mut self, buf: &mut Vec<u8>) -> std::io::Result<()>; // append raw bytes
    fn name(&self) -> &'static str;
}

struct DiagChar {
    fd: File,
    use_mdm: bool,
}

impl DiagChar {
    fn open(path: &str) -> std::io::Result<DiagChar> {
        let fd = File::options().read(true).write(true).open(path)?;
        let raw = fd.as_raw_fd();
        // SWITCH_LOGGING: try the 4-int param struct first, then the bare int.
        let mut param = [MEMORY_DEVICE_MODE as u32, (-1i32) as u32, 0u32, 0u32];
        let rc = unsafe {
            ioctl(raw, DIAG_IOCTL_SWITCH_LOGGING, param.as_mut_ptr(), 16usize, 0usize, 0usize)
        };
        if rc < 0 {
            let mode: i32 = MEMORY_DEVICE_MODE;
            let rc2 = unsafe { ioctl(raw, DIAG_IOCTL_SWITCH_LOGGING, mode, 0usize, 0usize, 0usize) };
            if rc2 < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        let mut use_mdm = 0i32;
        unsafe { ioctl(raw, DIAG_IOCTL_REMOTE_DEV, &mut use_mdm as *mut i32, 4usize, 0usize, 0usize) };
        Ok(DiagChar { fd, use_mdm: use_mdm == 1 })
    }
}

impl Transport for DiagChar {
    fn send(&mut self, cmd: u8, body: &[u8]) -> std::io::Result<()> {
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&USER_SPACE_DATA_TYPE.to_le_bytes());
        if self.use_mdm {
            pkt.extend_from_slice(&(-1i32).to_le_bytes());
        }
        pkt.extend_from_slice(&frame(cmd, body));
        (&mut self.fd).write_all(&pkt)
    }

    fn recv(&mut self, buf: &mut Vec<u8>) -> std::io::Result<()> {
        let mut chunk = [0u8; 16384];
        let n = (&mut self.fd).read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "diag eof"));
        }
        let data = &chunk[..n];
        if data.len() >= 4 && u32::from_le_bytes([data[0], data[1], data[2], data[3]]) == USER_SPACE_DATA_TYPE {
            // diagchar container: u32 type [(u32 mdm)] i32 nb { i32 sz, payload }
            let mut off = 4;
            if self.use_mdm {
                off += 4;
            }
            if data.len() >= off + 4 {
                let nb = i32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
                off += 4;
                for _ in 0..nb.max(0) {
                    if data.len() < off + 4 {
                        break;
                    }
                    let sz = i32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
                    off += 4;
                    if data.len() < off + sz {
                        break;
                    }
                    buf.extend_from_slice(&data[off..off + sz]);
                    off += sz;
                }
            }
            Ok(())
        } else {
            buf.extend_from_slice(data);
            Ok(())
        }
    }

    fn name(&self) -> &'static str {
        "diagchar"
    }
}

struct RawChannel {
    rd: File,
    wr: File,
}

impl RawChannel {
    fn open(rd_path: &str, wr_path: &str) -> std::io::Result<RawChannel> {
        Ok(RawChannel {
            rd: File::open(rd_path)?,
            wr: File::options().write(true).open(wr_path)?,
        })
    }
}

impl Transport for RawChannel {
    fn send(&mut self, cmd: u8, body: &[u8]) -> std::io::Result<()> {
        (&mut self.wr).write_all(&frame(cmd, body))
    }
    fn recv(&mut self, buf: &mut Vec<u8>) -> std::io::Result<()> {
        let mut chunk = [0u8; 16384];
        let n = (&mut self.rd).read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "raw eof"));
        }
        buf.extend_from_slice(&chunk[..n]);
        Ok(())
    }
    fn name(&self) -> &'static str {
        "raw"
    }
}

// ---------- little-endian readers ----------

fn u16le(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2).map(|s| u16::from(s[0]) | (u16::from(s[1]) << 8))
}
fn u32le(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from(s[0]) | (u32::from(s[1]) << 8) | (u32::from(s[2]) << 16) | (u32::from(s[3]) << 24))
}
fn u64le(b: &[u8], off: usize) -> Option<u64> {
    let mut v = 0u64;
    for i in 0..8 {
        v |= u64::from(*b.get(off + i)? ) << (8 * i);
    }
    Some(v)
}

// ---------- NR RRC Serving Cell Info (0xB823) ----------
// body (after 16-byte log header): rel_min(2) rel_maj(2), then per version.
// v0.4 @4:   pci(H) dl(I) ul(I) dlbw(H) ulbw(H) cellid(Q) mcc(H) mncdig(B)
//            mnc(H) access(B) tac(I) band(H)
// v3.x adds nr_cgi(Q) after pci: pci(H) cgi(Q) dl(I) ul(I) ... tac(I) band(H)
//   v3.0 @4 | v3.2/3.3 @7 (3B skip) | v3.4 @8 (4B prefix)

fn parse_nr_scell(body: &[u8]) -> Option<(u32, u32, u32, u32, u64)> {
    fn read_v4(b: &[u8], off: usize) -> Option<(u32, u32, u32, u32, u64)> {
        let pci = u32::from(u16le(b, off)?);
        let dl = u32le(b, off + 2)?;
        let cell_id = u64le(b, off + 14)?;
        let tac = u32le(b, off + 28)?;
        let band = u32::from(u16le(b, off + 32)?);
        Some((pci, dl, band, tac, cell_id))
    }
    fn read_v3(b: &[u8], off: usize) -> Option<(u32, u32, u32, u32, u64)> {
        let pci = u32::from(u16le(b, off)?);
        let dl = u32le(b, off + 10)?;
        let cell_id = u64le(b, off + 22)?;
        let tac = u32le(b, off + 36)?;
        let band = u32::from(u16le(b, off + 40)?);
        Some((pci, dl, band, tac, cell_id))
    }
    let rel_min = u16le(body, 0)?;
    let rel_maj = u16le(body, 2)?;
    if rel_maj == 0x00 && rel_min == 0x04 {
        read_v4(body, 4)
    } else if rel_maj == 0x03 {
        match rel_min {
            0x00 => read_v3(body, 4),
            0x04 => read_v3(body, 8),
            0x02 | 0x03 => read_v3(body, 7),
            _ => None,
        }
    } else {
        None
    }
}

// ---------- NR ML1 Meas DB Update (0xB97F) raster ARFCN ----------
// v2.7: <BB2sII> @4 (16B); v2.9/2.10/3.0: <IBBHII> @4 (20B); carrier rows
// carry raster_arfcn (u32) and serv_cell_pci (u16).

fn parse_nr_ml1_raster(body: &[u8]) -> Option<(u32, u32)> {
    let rel_min = u16le(body, 0)?;
    let rel_maj = u16le(body, 2)?;
    let (num_layers, off) = if rel_maj == 0x02 && rel_min == 0x07 {
        (u16le(body, 6)?, 16usize)
    } else if (rel_maj == 0x02 && (rel_min == 0x09 || rel_min == 0x0a)) || (rel_maj == 0x03 && rel_min == 0x00) {
        (u16le(body, 8)?, 20usize)
    } else {
        return None;
    };
    if num_layers == 0 {
        return None;
    }
    let row = if rel_maj == 0x02 { 32usize } else { 40usize };
    // v2.x row: raster_arfcn(I) cc? num_cells(B) serv_index(B) serv_pci(H)...
    let raster = u32le(body, off)?;
    let pci = u32::from(u16le(body, off + 8)?);
    let _ = num_layers;
    if raster > 0 && raster < MAX_ARFCN {
        return Some((raster, pci));
    }
    let _ = row;
    None
}

// ---------- publisher ----------

struct Published {
    arfcn: u32,
    pci: u32,
    band: u32,
}

fn setprop(name: &str, value: &str) {
    let _ = Command::new("/system/bin/setprop")
        .arg(name)
        .arg(value)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn publish(cur: &mut Option<Published>, pci: u32, arfcn: u32, band: u32, verbose: bool) {
    let changed = match cur {
        Some(c) => c.arfcn != arfcn || c.pci != pci || c.band != band,
        None => true,
    };
    if !changed {
        return;
    }
    *cur = Some(Published { arfcn, pci, band });
    setprop(&format!("{}arfcn", PROP_PREFIX), &arfcn.to_string());
    setprop(&format!("{}pci", PROP_PREFIX), &pci.to_string());
    setprop(&format!("{}band", PROP_PREFIX), &band.to_string());
    setprop(&format!("{}ts", PROP_PREFIX), &boottime_ms().to_string());
    if verbose {
        println!("[nsa5gfix-diagd] published arfcn={} pci={} band={}", arfcn, pci, band);
    }
}

// ---------- LOG_CONFIG ----------

fn log_config_enable(tr: &mut dyn Transport, verbose: bool) -> Result<(), String> {
    // Retrieve valid log-code ranges per type.
    let mut body = Vec::new();
    body.extend_from_slice(&[0u8; 3]); // pad
    body.extend_from_slice(&LOG_CONFIG_RETRIEVE_ID_RANGES_OP.to_le_bytes());
    tr.send(DIAG_LOG_CONFIG_F, &body).map_err(|e| e.to_string())?;

    let mut raw = Vec::new();
    let mut rsp = Vec::new();
    for _ in 0..20 {
        raw.clear();
        tr.recv(&mut raw).map_err(|e| e.to_string())?;
        for f in split_frames(&raw) {
            if f[0] == DIAG_LOG_CONFIG_F {
                rsp = f;
            } else {
                handle_frame(&f, &mut None, false);
            }
        }
        if !rsp.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if rsp.is_empty() {
        return Err("no LOG_CONFIG response".into());
    }
    if rsp.len() < 12 + 64 {
        return Err(format!("short LOG_CONFIG response: {} bytes", rsp.len()));
    }
    let op = u32le(&rsp, 4).unwrap_or(0);
    let status = u32le(&rsp, 8).unwrap_or(1);
    if op != LOG_CONFIG_RETRIEVE_ID_RANGES_OP || status != 0 {
        return Err(format!("LOG_CONFIG retrieve op={} status={}", op, status));
    }
    let bitsize = u32le(&rsp, 12 + 4 * 0x0b).unwrap_or(0);
    if bitsize == 0 || bitsize > 65536 * 8 {
        return Err(format!("type 0xB bitsize unusable: {}", bitsize));
    }
    if verbose {
        println!("[nsa5gfix-diagd] log type 0xB mask bits: {}", bitsize);
    }

    // Build the set-mask request for type 0xB.
    let want: [u16; 3] = [LOG_LTE_RRC_SERVING_CELL, LOG_NR_RRC_SERVING_CELL, LOG_NR_ML1_MEAS_DB];
    let nbytes = (bitsize as usize + 7) / 8;
    let mut mask = vec![0u8; nbytes];
    for code in want {
        let item = (code & 0x0fff) as usize;
        if item < nbytes * 8 {
            mask[item / 8] |= 1 << (item % 8);
        }
    }
    let mut body = Vec::new();
    body.extend_from_slice(&[0u8; 3]);
    body.extend_from_slice(&LOG_CONFIG_SET_MASK_OP.to_le_bytes());
    body.extend_from_slice(&0x0bu32.to_le_bytes());
    body.extend_from_slice(&bitsize.to_le_bytes());
    body.extend_from_slice(&mask);
    tr.send(DIAG_LOG_CONFIG_F, &body).map_err(|e| e.to_string())?;

    for _ in 0..20 {
        raw.clear();
        tr.recv(&mut raw).map_err(|e| e.to_string())?;
        for f in split_frames(&raw) {
            if f[0] == DIAG_LOG_CONFIG_F {
                let op = u32le(&f, 4).unwrap_or(0);
                let status = u32le(&f, 8).unwrap_or(1);
                if op == LOG_CONFIG_SET_MASK_OP && status == 0 {
                    if verbose {
                        println!("[nsa5gfix-diagd] log masks set for type 0xB");
                    }
                    return Ok(());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("no LOG_CONFIG set-mask confirmation".into())
}

fn handle_frame(f: &[u8], cur: &mut Option<Published>, verbose: bool) {
    if f.is_empty() {
        return;
    }
    if f[0] != DIAG_LOG_F || f.len() < 16 {
        return;
    }
    let log_id = u16le(f, 6).unwrap_or(0);
    let body = &f[16..];
    match log_id {
        LOG_NR_RRC_SERVING_CELL => match parse_nr_scell(body) {
            Some((pci, dl, band, tac, cell)) => {
                if verbose {
                    println!(
                        "[nsa5gfix-diagd] NR scell: pci={} dl_arfcn={} band={} tac={:x} cid={:x} (body {}B)",
                        pci, dl, band, tac, cell, body.len()
                    );
                }
                if dl > 0 && dl < MAX_ARFCN {
                    publish(cur, pci, dl, band, verbose);
                }
            }
            None => {
                let (rel_min, rel_maj) = (u16le(body, 0), u16le(body, 2));
                println!(
                    "[nsa5gfix-diagd] unknown 0xB823 version {:?}.{:?}: {}",
                    rel_min,
                    rel_maj,
                    hex(body.iter().take(64))
                );
            }
        },
        LOG_NR_ML1_MEAS_DB => {
            if let Some((raster, pci)) = parse_nr_ml1_raster(body) {
                if verbose {
                    println!("[nsa5gfix-diagd] NR ML1 raster arfcn={} pci={}", raster, pci);
                }
                setprop(&format!("{}ssb_raster", PROP_PREFIX), &raster.to_string());
                if cur.is_none() {
                    publish(cur, pci, raster, 0, verbose);
                }
            }
        }
        LOG_LTE_RRC_SERVING_CELL => {
            if verbose {
                let earfcn = u32le(body, 12).unwrap_or(0);
                println!("[nsa5gfix-diagd] LTE scell (anchor) earfcn~{}", earfcn);
            }
        }
        _ => {}
    }
}

fn hex<'a>(it: impl Iterator<Item = &'a u8>) -> String {
    it.map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join("")
}

fn usage() {
    eprintln!(
        "usage: nsa5gfix-diagd [--channel diag|smdcntl|ffs:RD,WR] [--verbose]\n\
         \x20      nsa5gfix-diagd probe   (report candidate DIAG channels)\n\
         \x20      nsa5gfix-diagd selftest"
    );
}

fn selftest() -> i32 {
    // Synthetic 0xB823 v3.4 frame end-to-end through framing + parsing.
    let mut body = Vec::new();
    body.extend_from_slice(&4u16.to_le_bytes()); // rel_min
    body.extend_from_slice(&3u16.to_le_bytes()); // rel_maj
    body.extend_from_slice(&[1, 0, 1, 0]); // v3.4 prefix
    body.extend_from_slice(&501u16.to_le_bytes()); // pci
    body.extend_from_slice(&0x1234567890abcdefu64.to_le_bytes()); // nr_cgi
    body.extend_from_slice(&431880u32.to_le_bytes()); // dl arfcn
    body.extend_from_slice(&432000u32.to_le_bytes()); // ul arfcn
    body.extend_from_slice(&100u16.to_le_bytes()); // dl bw
    body.extend_from_slice(&100u16.to_le_bytes()); // ul bw
    body.extend_from_slice(&0xdeadbeefcafeu64.to_le_bytes()); // cell id
    body.extend_from_slice(&257u16.to_le_bytes()); // mcc
    body.extend_from_slice(&2u8.to_le_bytes()); // mnc digits
    body.extend_from_slice(&2u16.to_le_bytes()); // mnc
    body.extend_from_slice(&1u8.to_le_bytes()); // allowed access
    body.extend_from_slice(&0x1a2bu32.to_le_bytes()); // tac
    body.extend_from_slice(&78u16.to_le_bytes()); // band

    let mut log = Vec::new();
    log.push(DIAG_LOG_F);
    log.push(0);
    log.extend_from_slice(&((body.len() + 4) as u16).to_le_bytes());
    log.extend_from_slice(&0u16.to_le_bytes());
    log.extend_from_slice(&LOG_NR_RRC_SERVING_CELL.to_le_bytes());
    log.extend_from_slice(&0u64.to_le_bytes());
    log.extend_from_slice(&body);

    let framed = frame(DIAG_LOG_F, &log[1..]); // wire form of the log packet
    let frames = split_frames(&framed);
    assert_eq!(frames.len(), 1, "one frame round-trips");
    let f = &frames[0];
    assert_eq!(f[0], DIAG_LOG_F);
    let got_id = u16le(f, 6).unwrap();
    assert_eq!(got_id, LOG_NR_RRC_SERVING_CELL, "log id survives framing");
    let (pci, dl, band, tac, _) = parse_nr_scell(&f[16..]).expect("v3.4 parses");
    assert_eq!((pci, dl, band, tac), (501, 431880, 78, 0x1a2b), "fields decode");
    println!("selftest ok: 0xB823 v3.4 pci=501 dl=431880 band=78 tac=1a2b");
    0
}

fn probe() -> i32 {
    for p in ["/dev/diag", "/dev/diag_mdm", "/dev/smdcntl8", "/dev/at_mdm0"] {
        match std::fs::metadata(p) {
            Ok(m) => println!("{}: exists (mode {:o})", p, m.permissions().mode()),
            Err(_) => println!("{}: absent", p),
        }
    }
    if let Ok(dirs) = std::fs::read_dir("/dev") {
        for d in dirs.flatten() {
            let name = d.file_name().to_string_lossy().to_string();
            if name.contains("diag") || name.contains("smdcntl") || name.contains("ffs") {
                println!("/dev/{}: present", name);
            }
        }
    }
    0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "selftest") {
        std::process::exit(selftest());
    }
    if args.iter().any(|a| a == "probe") {
        std::process::exit(probe());
    }
    let verbose = args.iter().any(|a| a == "--verbose");
    let channel = args
        .iter()
        .position(|a| a == "--channel")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "diag".to_string());

    let mut tr: Box<dyn Transport> = match channel.as_str() {
        "diag" => match DiagChar::open("/dev/diag") {
            Ok(t) => {
                println!("[nsa5gfix-diagd] opened /dev/diag (mdm field: {})", t.use_mdm);
                Box::new(t)
            }
            Err(e) => {
                eprintln!("[nsa5gfix-diagd] /dev/diag failed: {} (try: probe, --channel smdcntl, --channel ffs:RD,WR)", e);
                std::process::exit(2);
            }
        },
        "smdcntl" => match RawChannel::open("/dev/smdcntl8", "/dev/smdcntl8") {
            Ok(t) => {
                println!("[nsa5gfix-diagd] opened /dev/smdcntl8");
                Box::new(t)
            }
            Err(e) => {
                eprintln!("[nsa5gfix-diagd] /dev/smdcntl8 failed: {}", e);
                std::process::exit(2);
            }
        },
        s if s.starts_with("ffs:") => {
            let pair = s[4..].split(',').collect::<Vec<_>>();
            if pair.len() != 2 {
                eprintln!("[nsa5gfix-diagd] ffs channel needs RD,WR paths");
                std::process::exit(2);
            }
            match RawChannel::open(pair[0], pair[1]) {
                Ok(t) => {
                    println!("[nsa5gfix-diagd] opened {} / {}", pair[0], pair[1]);
                    Box::new(t)
                }
                Err(e) => {
                    eprintln!("[nsa5gfix-diagd] ffs open failed: {}", e);
                    std::process::exit(2);
                }
            }
        }
        _ => {
            usage();
            std::process::exit(2);
        }
    };

    if let Err(e) = log_config_enable(tr.as_mut(), verbose) {
        eprintln!("[nsa5gfix-diagd] LOG_CONFIG failed: {}", e);
        std::process::exit(3);
    }

    // The notification app's Stop action kills exactly this pid (root-side).
    setprop(&format!("{}pid", PROP_PREFIX), &std::process::id().to_string());

    let mut cur: Option<Published> = None;
    let mut raw: Vec<u8> = Vec::new();
    loop {
        raw.clear();
        if let Err(e) = tr.recv(&mut raw) {
            eprintln!("[nsa5gfix-diagd] recv error: {}", e);
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        for f in split_frames(&raw) {
            handle_frame(&f, &mut cur, verbose);
        }
    }
}
