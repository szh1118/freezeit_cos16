package io.github.jark006.freezeit.hook.android;

import io.github.jark006.freezeit.hook.Config;
import io.github.jark006.freezeit.hook.XpUtils;

/**
 * Wake-lock restrictions are applied by FreezeitService through AppOps only after the daemon
 * explicitly requests them. Do not short-circuit acquireWakeLockInternal here: managedApp is a
 * policy set, not a frozen-state or daemon-policy signal.
 */
public class WakeLockHook {
    final static String TAG = "唤醒锁";

    public WakeLockHook(Config config, ClassLoader classLoader) {
        XpUtils.log(TAG, "WakeLock acquisition is controlled by AppOps policy");
    }
}
