#!/usr/bin/env bash
# run.sh — deploy the NSA NR-ARFCN patch to the rooted SM-S936B.
#
# Prereqs:
#   * phone on adb (USB), unlocked, FRESH BOOT (one exploit attempt per boot,
#     ~10 min budget; the recorded script refuses in-boot retries)
#   * KernelSU Manager may show a superuser prompt — accept it
#   * host frida CLI installed (17.7.2; server binary ships in this dir)
#
# Everything is volatile: reboot clears root, the daemon and the hook.
set -uo pipefail

WS=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PAYLOADS=${PAYLOADS:-$HOME/Projects/code/Root-My-Galaxy-Payloads}
REMOTE=/data/local/tmp
CHANNEL=${CHANNEL:-diag}   # diag | smdcntl | ffs:RD,WR — set after recon

step() { printf '\n=== %s ===\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

step 'Preflight'
adb get-state >/dev/null 2>&1 || die 'no adb device; reconnect the phone'
[ -x "$WS/nsa5gfix-diagd" ] || die 'missing nsa5gfix-diagd (build: see NOTES.md)'
[ -x "$WS/frida-server" ] || die 'missing frida-server'
command -v frida >/dev/null 2>&1 || die 'host frida CLI not installed'

step 'Root (recorded device-tested procedure, ~10 min, one attempt)'
"$PAYLOADS/tools/root-pa2q-S936BXXUCDZIF.sh" || die 'root chain failed; reboot the phone before retrying'

step 'Push components'
adb push "$WS/nsa5gfix-diagd" "$REMOTE/nsa5gfix-diagd" || die 'push nsa5gfix-diagd failed'
adb push "$WS/frida-server" "$REMOTE/frida-server" || die 'push frida-server failed'
adb shell "chmod 755 $REMOTE/nsa5gfix-diagd $REMOTE/frida-server"

step 'Recon: candidate DIAG channels (as root)'
adb shell "su -c $REMOTE/nsa5gfix-diagd probe" || true
echo "hint: if /dev/diag is absent, retry this step's channel choice:"
echo "  CHANNEL=smdcntl ./run.sh   (or ffs:/dev/ffs-diag/epX,/dev/ffs-diag/epY)"

step "Start nsa5gfix-diagd (channel: $CHANNEL)"
adb shell "su -c 'nohup $REMOTE/nsa5gfix-diagd --channel $CHANNEL --verbose > $REMOTE/nsa5gfix-diagd.log 2>&1 &'"
sleep 2
adb shell "su -c 'tail -n 20 $REMOTE/nsa5gfix-diagd.log'" || true
grep -q 'log masks set' <(adb shell "su -c cat $REMOTE/nsa5gfix-diagd.log" 2>/dev/null) \
    || echo 'WARNING: masks not confirmed yet — check the log above.'

step 'Start frida-server (as root)'
adb shell "su -c 'nohup $REMOTE/frida-server -l 0.0.0.0:27042 > $REMOTE/frida-server.log 2>&1 &'" \
    || adb shell "su -c 'nohup $REMOTE/frida-server > $REMOTE/frida-server.log 2>&1 &'"
sleep 2

step 'Attach the hook to com.android.phone (Ctrl+C detaches; patch persists in-process)'
frida -U -n com.android.phone -l "$WS/nsa5gfix-hook.js"

printf '\nState (any time):\n'
printf '  props:  adb shell getprop | grep nsa5gfix\n'
printf '  daemon: adb shell "su -c tail /data/local/tmp/nsa5gfix-diagd.log"\n'
printf 'Under 5G NSA: open 4Cells — NR ARFCN (and PCI) should now be populated.\n'
