#!/usr/bin/env sh
set -eu

SERIAL_ARG="${1:-}"
ADB="${ADB:-adb}"
PACKAGE_LIST="${PACKAGE_LIST:-}"
FREEZE_WAIT_SECONDS="${FREEZE_WAIT_SECONDS:-15}"
if [ -z "$PACKAGE_LIST" ]; then
    echo "usage: PACKAGE_LIST='pkg.one pkg.two pkg.three' $0 [serial]" >&2
    exit 2
fi

case "$FREEZE_WAIT_SECONDS" in
    0|[1-9][0-9]*) ;;
    *)
        echo "FREEZE_WAIT_SECONDS must be a non-negative integer" >&2
        exit 2
        ;;
esac

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

freeze_state_for_pid() {
    # shellcheck disable=SC2016
    run_root_script '
uid=$1
pid=$2
for root in /sys/fs/cgroup/apps /sys/fs/cgroup/system; do
    path="$root/uid_$uid/pid_$pid/cgroup.freeze"
    if [ -r "$path" ]; then
        state=$(cat "$path") || exit 1
        case "$state" in
            0|1)
                printf "cgroup:%s\\n" "$state"
                exit 0
                ;;
            *)
                exit 1
                ;;
        esac
    fi
done

if [ -r "/proc/$pid/status" ]; then
    while IFS= read -r line; do
        case "$line" in
            State:*)
                state=${line#State:}
                set -- $state
                [ "$#" -gt 0 ] || exit 1
                printf "process:%s\\n" "$1"
                exit 0
                ;;
        esac
    done < "/proc/$pid/status"
fi
exit 1
' "$1" "$2"
}

wait_for_running() {
    uid=$1
    pid=$2
    phase=$3
    remaining=$FREEZE_WAIT_SECONDS

    while :; do
        if state="$(freeze_state_for_pid "$uid" "$pid" 2>/dev/null)"; then
            case "$state" in
                cgroup:0|process:[!Tt]*)
                    echo "$phase package_state=$state pid=$pid"
                    return 0
                    ;;
            esac
        fi
        [ "$remaining" -gt 0 ] || break
        sleep 1
        remaining=$((remaining - 1))
    done

    echo "$phase package_state=not_running pid=$pid" >&2
    return 1
}

wait_for_frozen() {
    uid=$1
    pid=$2
    remaining=$FREEZE_WAIT_SECONDS

    while :; do
        if state="$(freeze_state_for_pid "$uid" "$pid" 2>/dev/null)"; then
            case "$state" in
                cgroup:1|process:T|process:t)
                    echo "frozen package_state=$state pid=$pid"
                    return 0
                    ;;
            esac
        fi
        [ "$remaining" -gt 0 ] || break
        sleep 1
        remaining=$((remaining - 1))
    done

    echo "frozen package_state=not_observed pid=$pid" >&2
    return 1
}

echo "freezeit freeze/unfreeze validation"
echo "timestamp=$(date '+%Y-%m-%d %H:%M:%S')"
echo "packages=$PACKAGE_LIST"

if [ "$(run_device "su -c 'ss -xl 2>/dev/null | grep -q FreezeitManager && echo reachable || echo unreachable'")" != "reachable" ]; then
    echo "daemon_socket=unreachable"
    exit 1
fi
echo "daemon_socket=reachable"

# PACKAGE_LIST is whitespace-separated. Disable pathname expansion so an arbitrary
# element remains one argument when it is encoded for the remote shell.
set -f
package_count=0
# shellcheck disable=SC2086
for package_name in $PACKAGE_LIST; do
    package_count=$((package_count + 1))
    package_arg="$(shell_quote "$package_name")"
    package_info="$(run_device "cmd package list packages -U $package_arg 2>/dev/null")" || {
        echo "package=$package_name query_failed" >&2
        exit 1
    }
    case "$package_info" in
        *"package:$package_name uid:"*) ;;
        *)
            echo "package=$package_name missing" >&2
            exit 1
            ;;
    esac
    uid="$(printf '%s\n' "$package_info" | sed -n 's/.* uid:\([0-9][0-9]*\).*/\1/p' | head -n 1)"
    case "$uid" in
        ''|*[!0-9]*)
            echo "package=$package_name invalid_uid=${uid:-unknown}" >&2
            exit 1
            ;;
    esac
    echo "package=$package_name uid=$uid"

    run_device "am force-stop $package_arg"
    run_device "monkey -p $package_arg -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1"
    sleep 2
    foreground_pids="$(run_device "pidof $package_arg 2>/dev/null || true")"
    [ -n "$foreground_pids" ] || {
        echo "package=$package_name foreground_pid=missing" >&2
        exit 1
    }
    echo "foreground_pid=$foreground_pids"
    # shellcheck disable=SC2086
    for pid in $foreground_pids; do
        case "$pid" in
            ''|*[!0-9]*)
                echo "package=$package_name invalid_pid=$pid" >&2
                exit 1
                ;;
        esac
        wait_for_running "$uid" "$pid" initial
    done

    run_device "input keyevent KEYCODE_HOME"
    # shellcheck disable=SC2086
    for pid in $foreground_pids; do
        wait_for_frozen "$uid" "$pid"
    done

    run_device "monkey -p $package_arg -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1"
    # shellcheck disable=SC2086
    for pid in $foreground_pids; do
        wait_for_running "$uid" "$pid" unfrozen
    done
    echo "package=$package_name freeze_unfreeze=verified"
done

[ "$package_count" -gt 0 ] || {
    echo "PACKAGE_LIST contains no package names" >&2
    exit 2
}
