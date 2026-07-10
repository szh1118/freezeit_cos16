#!/system/bin/sh
MODDIR=${0%/*}

bootLogPath=$MODDIR/boot.log

# This function is copied from [ Uperf@yc9559 ] module.
wait_until_login() {

    # in case of /data encryption is disabled
    while [ "$(getprop sys.boot_completed)" != "1" ]; do
        sleep 5
    done

    # we doesn't have the permission to rw "/sdcard" before the user unlocks the screen
    # shellcheck disable=SC2039
    local test_file="/sdcard/Android/.PERMISSION_TEST_FREEZEIT"
    true >"$test_file"
    while [ ! -f "$test_file" ]; do
        sleep 5
        true >"$test_file"
    done
    rm "$test_file"
}

echo "[$(date "+%Y-%m-%d %H:%M:%S")] 开始运行服务脚本" >"$bootLogPath"

wait_until_login

echo "[$(date "+%Y-%m-%d %H:%M:%S")] 进入桌面, 10秒后将启动冻它" >>"$bootLogPath"

sleep 10

if [ -e "$MODDIR"/disable ] || [ -e "$MODDIR"/remove ]; then
    # shellcheck disable=SC2086
    echo "[$(date "+%Y-%m-%d %H:%M:%S")] 冻它已被禁用或移除，取消启动" >>$bootLogPath
    exit
fi

echo "[$(date "+%Y-%m-%d %H:%M:%S")] 启动冻它" >>"$bootLogPath"

baseline="$MODDIR/rom_baseline.prop"
if [ -f "$baseline" ]; then
    baseline_fingerprint="$(sed -n 's/^rom.build.fingerprint=//p' "$baseline" | head -n 1)"
    baseline_incremental="$(sed -n 's/^rom.build.incremental=//p' "$baseline" | head -n 1)"
    device_fingerprint="$(getprop ro.build.fingerprint)"
    device_incremental="$(getprop ro.build.version.incremental)"

    if [ -n "$baseline_fingerprint" ] && [ -n "$device_fingerprint" ] && [ "$baseline_fingerprint" != "$device_fingerprint" ]; then
        echo "[$(date "+%Y-%m-%d %H:%M:%S")] WARNING ROM fingerprint mismatch; continuing startup" >>"$bootLogPath"
        echo "  baseline=$baseline_fingerprint" >>"$bootLogPath"
        echo "  device=$device_fingerprint" >>"$bootLogPath"
    fi

    if [ -n "$baseline_incremental" ] && [ -n "$device_incremental" ] && [ "$baseline_incremental" != "$device_incremental" ]; then
        echo "[$(date "+%Y-%m-%d %H:%M:%S")] WARNING ROM incremental mismatch; continuing startup baseline=$baseline_incremental device=$device_incremental" >>"$bootLogPath"
    fi
fi

if [ ! -x "$MODDIR"/freezeit ]; then
    echo "[$(date "+%Y-%m-%d %H:%M:%S")] 冻它 daemon binary missing or not executable" >>"$bootLogPath"
    exit 1
fi

# 带一个任意参数将开启文件式日志 [ /sdcard/Android/freezeit.log ]
# "$MODDIR"/freezeit 0
exec "$MODDIR"/freezeit
