$BOOTMODE || abort "- 🚫 安装失败，仅支持在 Magisk 或 KernelSU 下安装"

kernelVersionCode=$(uname -r |awk -F '.' '{print $1*100+$2}')
if [ $kernelVersionCode -lt 510 ];then
    echo "- 🚫 安装失败，仅支持内核版本 5.10 或以上"
    echo "- 🚫 本机内核版本 $(uname -r)"
    abort
fi

[ "$API" -ge 31 ] || abort "- 🚫 安装失败，仅支持 安卓12 或以上"

[ "$ARCH" = "arm64" ] || abort "- 🚫 安装失败：Rust daemon 仅支持 ARM64，当前架构: $ARCH"

for forbidden_daemon in freezeitARM64 freezeitX64 freezeitRustARM64 freezeitRustX64; do
    [ ! -e "$MODPATH/$forbidden_daemon" ] || abort "- 🚫 安装包包含已禁止的 daemon: $forbidden_daemon"
done

[ -f "$MODPATH/freezeit" ] || abort "- 🚫 安装包缺少唯一 Rust daemon: freezeit"
[ -f "$MODPATH/SHA256SUMS" ] || abort "- 🚫 安装包缺少 SHA256SUMS"
(cd "$MODPATH" && sha256sum -c SHA256SUMS) || abort "- 🚫 安装包 SHA256 校验失败"

chmod a+x "$MODPATH"/freezeit
chmod a+x "$MODPATH"/service.sh

output=$(pm uninstall cn.myflv.android.noanr)
if [ "$output" == "Success" ]; then
    echo "- ⚠️功能冲突, 已卸载 [NoANR]"
fi

output=$(pm list packages cn.myflv.android.noactive)
if [ ${#output} -gt 2 ]; then
    echo "- ⚠️检测到 [NoActive](myflavor), 请到 LSPosed 将其禁用"
fi

output=$(pm list packages com.github.uissd.miller)
if [ ${#output} -gt 2 ]; then
    echo "- ⚠️检测到 [Miller](UISSD), 请到 LSPosed 将其禁用"
fi

output=$(pm list packages com.github.f19f.milletts)
if [ ${#output} -gt 2 ]; then
    echo "- ⚠️检测到 [MiTombstone](f19没有新欢), 请到 LSPosed 将其禁用"
fi

output=$(pm list packages com.ff19.mitlite)
if [ ${#output} -gt 2 ]; then
    echo "- ⚠️检测到 [Mitlite](f19没有新欢), 请到 LSPosed 将其禁用"
fi

output=$(pm list packages com.sidesand.millet)
if [ ${#output} -gt 2 ]; then
    echo "- ⚠️检测到 [SMillet](酱油一下下), 请到 LSPosed 将其禁用"
fi

output=$(pm list packages com.mubei.android)
if [ ${#output} -gt 2 ]; then
    echo "- ⚠️检测到 [墓碑](离音), 请到 LSPosed 将其禁用"
fi

if [ -e "/data/adb/modules/mubei" ]; then
    echo "- ⚠️已禁用 [自动墓碑后台](奋斗的小青年)"
    touch /data/adb/modules/mubei/disable
fi

if [ -e "/data/adb/modules/Hc_tombstone" ]; then
    echo "- ⚠️已禁用 [新内核墓碑](时雨星空/火柴)"
    touch /data/adb/modules/Hc_tombstone/disable
fi

ORG_appcfg="/data/adb/modules/freezeit/appcfg.txt"
ORG_applabel="/data/adb/modules/freezeit/applabel.txt"
ORG_settings="/data/adb/modules/freezeit/settings.db"

for path in $ORG_appcfg $ORG_applabel $ORG_settings; do
    if [ -e $path ]; then
        cp -f $path "$MODPATH"
    fi
done

output=$(pm list packages io.github.jark006.freezeit)
if [ ${#output} -lt 2 ]; then
    echo "- ⚠️ 首次安装, 安装完毕后, 请到LSPosed管理器启用冻它, 然后再重启"
fi

module_version="$(grep_prop version "$MODPATH"/module.prop)"
echo "- 正在安装 $module_version"

fullApkPath=$(ls "$MODPATH"/freezeit*.apk)
apkPath=/data/local/tmp/freezeit.apk
mv -f "$fullApkPath" "$apkPath"
chmod 666 "$apkPath"

echo "- 冻它APP 正在安装..."
output=$(pm install -r -f "$apkPath" 2>&1)
if [ "$output" == "Success" ]; then
    echo "- 冻它APP 安装成功"
    rm -rf "$apkPath"
else
    echo "- 冻它APP 安装失败, 原因: [$output] 尝试卸载再安装..."
    pm uninstall io.github.jark006.freezeit
    sleep 1
    output=$(pm install -r -f "$apkPath" 2>&1)
    if [ "$output" == "Success" ]; then
        echo "- 冻它APP 安装成功"
        echo "- ⚠️请到LSPosed管理器重新启用冻它, 然后再重启"
        rm -rf "$apkPath"
    else
        apkPathSdcard="/sdcard/freezeit_${module_version}.apk"
        cp -f "$apkPath" "$apkPathSdcard"
        echo "*********************** !!!"
        echo "  冻它APP 依旧安装失败, 原因: [$output]"
        echo "  请手动安装 [ $apkPathSdcard ]"
        echo "*********************** !!!"
    fi
fi

# 仅限 MIUI 12~14, HyperOS 1~6
MIUI_VersionCode=$(getprop ro.miui.ui.version.code)
HyperOS_VersionCode=$(getprop ro.mi.os.version.code)
if [ "$MIUI_VersionCode" -ge 12 ] && [ "$MIUI_VersionCode" -le 14 ]; then
    echo "- 已配置禁用Millet参数  MIUI $MIUI_VersionCode"
elif [ "$HyperOS_VersionCode" -ge 1 ] && [ "$HyperOS_VersionCode" -le 6 ]; then
    echo "- 已配置禁用Millet参数  HyperOS $HyperOS_VersionCode"
else
    rm "$MODPATH"/system.prop
fi

echo ""
cat "$MODPATH"/changelog.txt
echo ""
echo "- 安装完毕, 重启生效"
echo "- 若出现以下异常日志文件, 请反馈给作者, 谢谢"
echo "- [ /sdcard/Android/freezeit_crash_log.txt ]"
echo ""
