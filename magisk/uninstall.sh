#!/system/bin/sh

# This function is copied from [ Uperf@yc9559 ] module.
wait_until_login() {
    # in case of /data encryption is disabled
    while [ "$(getprop sys.boot_completed)" != "1" ]; do
        sleep 1
    done

    # we doesn't have the permission to rw "/sdcard" before the user unlocks the screen
    # shellcheck disable=SC2039
    local test_file="/sdcard/Android/.PERMISSION_TEST_FREEZEIT"
    while :; do
        rm -f "$test_file" 2>/dev/null
        if [ -e "$test_file" ]; then
            sleep 1
            continue
        fi

        if true >"$test_file" 2>/dev/null &&
                [ -f "$test_file" ] &&
                rm -f "$test_file" 2>/dev/null &&
                [ ! -e "$test_file" ]; then
            break
        fi

        rm -f "$test_file" 2>/dev/null
        sleep 1
    done
}

remove_freezeit(){
    wait_until_login

    pm uninstall io.github.jark006.freezeit
    rm -f /sdcard/Android/.PERMISSION_TEST_FREEZEIT
    rm -f /sdcard/Android/freezeit_crash_log.txt
    rm -f /sdcard/Android/freezeit.log
}

(remove_freezeit &)
