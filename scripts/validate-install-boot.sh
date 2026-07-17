#!/usr/bin/env sh
set -eu

SERIAL_ARG="${1:-}"
ADB="${ADB:-adb}"
MODDIR="${MODDIR:-/data/adb/modules/freezeit}"

if [ -n "$SERIAL_ARG" ] && command -v "$ADB" >/dev/null 2>&1; then
    USE_ADB=1
elif command -v getprop >/dev/null 2>&1; then
    USE_ADB=0
else
    USE_ADB=1
fi

run_device() {
    if [ "$USE_ADB" -eq 1 ]; then
        if [ -n "$SERIAL_ARG" ]; then
            "$ADB" -s "$SERIAL_ARG" shell "$1"
        else
            "$ADB" shell "$1"
        fi
    else
        sh -c "$1"
    fi
}

shell_quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

run_root_script() {
    root_script=$1
    shift

    root_command="sh -c $(shell_quote "$root_script") sh"
    for root_arg in "$@"; do
        root_command="$root_command $(shell_quote "$root_arg")"
    done
    run_device "su -c $(shell_quote "$root_command")"
}

echo "freezeit install boot validation"
echo "timestamp=$(date '+%Y-%m-%d %H:%M:%S')"
echo "boot_completed=$(run_device 'getprop sys.boot_completed')"
echo "module_dir=$MODDIR"

# shellcheck disable=SC2016
if run_root_script 'test -d "$1"' "$MODDIR"; then
    echo "module_dir=present"
else
    echo "module_dir=missing"
    exit 1
fi

# shellcheck disable=SC2016
if run_root_script 'test -e "$1/disable" || test -e "$1/remove"' "$MODDIR"; then
    echo "module_state=disabled_or_remove_pending"
    exit 1
fi

# shellcheck disable=SC2016
if run_root_script 'test -x "$1/freezeit"' "$MODDIR"; then
    echo "daemon_binary=executable"
else
    echo "daemon_binary=missing_or_not_executable"
    exit 1
fi

if [ "$(run_device "ss -xl 2>/dev/null | grep -q 'FreezeitManager' && echo reachable || echo unreachable")" = "reachable" ]; then
    echo "daemon_socket=reachable"
else
    echo "daemon_socket=unreachable"
    exit 1
fi

# shellcheck disable=SC2016
if run_root_script 'test -f "$1/boot.log"' "$MODDIR"; then
    echo "boot_log=present"
    # shellcheck disable=SC2016
    run_root_script 'tail -n 20 "$1/boot.log"' "$MODDIR"
else
    echo "boot_log=missing"
    exit 1
fi
