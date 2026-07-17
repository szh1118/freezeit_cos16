#!/usr/bin/env sh
set -eu

SERIAL_ARG="${1:-}"
ADB="${ADB:-adb}"

run_shell() {
  if [ -n "$SERIAL_ARG" ]; then
    "$ADB" -s "$SERIAL_ARG" shell "$1"
  else
    "$ADB" shell "$1"
  fi
}

echo "== freezeit degraded-state read-only validation =="
run_shell 'getprop sys.boot_completed'
run_shell 'su -c "id -u"'
run_shell 'su -c "pidof freezeit || true"'
run_shell 'su -c "ss -xl | grep FreezeitManager || true"'
run_shell 'pm list packages -U | head -5'
run_shell 'su -c "test -e /data/adb/modules/freezeit/appcfg.txt && echo policy-ready || echo policy-missing"'
run_shell 'su -c "find /sys/fs/cgroup/apps /sys/fs/cgroup/system -name cgroup.freeze -type f 2>/dev/null | head -1 | grep -q . && echo cgroup-freezer-present || echo cgroup-freezer-missing"'
run_shell 'su -c "test -e /dev/binderfs/binder-control -o -e /dev/binder && echo binder-present || echo binder-missing"'
run_shell 'dumpsys power | grep -E "mWakefulness|Display Power" | head -5 || true'
run_shell 'dumpsys connectivity | head -5 || true'
