package io.github.jark006.freezeit.hook.android;

import static io.github.jark006.freezeit.hook.XpUtils.log;

import android.annotation.SuppressLint;

import java.util.ArrayList;

import io.github.jark006.freezeit.hook.Config;
import io.github.jark006.freezeit.hook.Enum;
import io.github.jark006.freezeit.hook.XpUtils;
import io.github.jark006.freezeit.hook.XpUtils.MethodHook;
import io.github.jark006.freezeit.hook.XpUtils.MethodHookParam;

public class AlarmHook {
    final static String TAG = "定时器";
    Config config;

    public AlarmHook(Config config, ClassLoader classLoader) {
        this.config = config;

        XpUtils.hookMethod(TAG, classLoader, callback,
                Enum.Class.AlarmManagerServiceS,
                Enum.Method.triggerAlarmsLocked, ArrayList.class, long.class);
    }

    // SDK S-T A12-13
    // https://cs.android.com/android/platform/superproject/+/android-12.0.0_r34:frameworks/base/apex/jobscheduler/service/java/com/android/server/alarm/AlarmManagerService.java;l=3870
    // int triggerAlarmsLocked(ArrayList<Alarm> triggerList, final long nowELAPSED)

    // SDK x ~ R
    // https://cs.android.com/android/platform/superproject/+/android-11.0.0_r48:frameworks/base/services/core/java/com/android/server/AlarmManagerService.java;l=3499
    // https://cs.android.com/android/platform/superproject/+/android-10.0.0_r47:frameworks/base/services/core/java/com/android/server/AlarmManagerService.java;l=3469
    MethodHook callback = new MethodHook() {
        @SuppressLint("DefaultLocale")
        public void afterHookedMethod(MethodHookParam param) {
            // Alarm
            // SDK31 https://cs.android.com/android/platform/superproject/+/android-12.0.0_r34:frameworks/base/apex/jobscheduler/service/java/com/android/server/alarm/Alarm.java
            // SDK30 https://cs.android.com/android/platform/superproject/+/android-11.0.0_r48:frameworks/base/services/core/java/com/android/server/AlarmManagerService.java;l=3636
            var iterator = ((ArrayList<?>) param.args[0]).iterator(); //triggerList
            while (iterator.hasNext()) {
                final var Alarm = iterator.next(); //迭代器后移，再返回新位置的元素
                final int uid = config.getAlarmUid(Alarm);
                // Until foreground and pending-freeze snapshots are both known, preserve the
                // alarm. A managed UID alone does not prove that its process is frozen.
                if (!FreezeitService.shouldSuppressBackgroundWork(config, uid))
                    continue;

                iterator.remove();
                if (XpUtils.DEBUG_ALARM)
                    log(TAG, "清理 " + XpUtils.getString(Alarm, Enum.Field.packageName) +
                            " statsTag:" + XpUtils.getString(Alarm, "statsTag"));
            }
        }
    };
}
