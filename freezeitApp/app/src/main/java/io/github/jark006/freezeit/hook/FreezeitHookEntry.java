package io.github.jark006.freezeit.hook;

import static io.github.jark006.freezeit.hook.XpUtils.log;

import android.app.Application;
import android.os.Build;

import io.github.jark006.freezeit.BuildConfig;
import io.github.jark006.freezeit.hook.android.AlarmHook;
import io.github.jark006.freezeit.hook.android.AnrHook;
import io.github.jark006.freezeit.hook.android.BroadCastHook;
import io.github.jark006.freezeit.hook.android.FreezeitService;
import io.github.jark006.freezeit.hook.android.WakeLockHook;
import io.github.jark006.freezeit.hook.app.OplusAthena;
import io.github.jark006.freezeit.hook.app.PowerKeeper;

public final class FreezeitHookEntry {
    private static final int ANDROID_16_API_LEVEL = 36;
    private static final String ATHENA_TAG = "Freezeit[OplusAthena]:";
    private static final String ATHENA_SIGNATURE_HOOK_ID = "athena#signature_selection";
    private static final String MODERN_BROADCAST_HOOK_ID = "broadcast#modern_queue";
    private static final Object ATHENA_INSTALL_LOCK = new Object();

    private FreezeitHookEntry() {
    }

    public static void handlePackage(String packageName, ClassLoader classLoader) {
        switch (packageName) {
            case Enum.Package.self:
                XpUtils.hookMethod("Freezeit[manager]:", classLoader,
                        XpUtils.returnConstant(true),
                        Enum.Class.self, Enum.Method.isXposedActive);
                return;
            case Enum.Package.android:
                hookAndroid(classLoader);
                return;
            case Enum.Package.powerkeeper:
                PowerKeeper.Hook(classLoader);
                return;
            case Enum.Package.oplusAthena:
                OplusAthena.Hook(classLoader);
                return;
            default:
        }
    }

    /**
     * ActivityThread.currentApplication() is populated before Instrumentation calls the
     * application's onCreate(). Installing here makes Athena identity selection reliable while
     * still installing its hooks before Athena application code can trigger cleanup work.
     */
    public static void hookAthenaWhenApplicationReady(final ClassLoader classLoader) {
        XpUtils.hookMethod(ATHENA_TAG, classLoader, new XpUtils.MethodHook() {
                    @Override
                    protected void beforeHookedMethod(XpUtils.MethodHookParam param) {
                        installAthenaHooksBeforeOnCreate(classLoader);
                    }
                }, Enum.Class.Instrumentation, Enum.Method.callApplicationOnCreate,
                Application.class);
    }

    private static void installAthenaHooksBeforeOnCreate(ClassLoader classLoader) {
        if (HookHealthRegistry.hasSuccessfulRegistration(ATHENA_SIGNATURE_HOOK_ID)) {
            return;
        }
        if (!isAthenaApplicationReady()) {
            HookHealthRegistry.declareHook(ATHENA_SIGNATURE_HOOK_ID, true);
            HookHealthRegistry.recordRegistrationFailure(ATHENA_SIGNATURE_HOOK_ID,
                    new IllegalStateException("currentApplication unavailable before onCreate"));
            return;
        }
        synchronized (ATHENA_INSTALL_LOCK) {
            if (!HookHealthRegistry.hasSuccessfulRegistration(ATHENA_SIGNATURE_HOOK_ID)) {
                OplusAthena.Hook(classLoader);
            }
        }
    }

    private static boolean isAthenaApplicationReady() {
        try {
            Application application = (Application) Class.forName("android.app.ActivityThread")
                    .getDeclaredMethod("currentApplication").invoke(null);
            return application != null;
        } catch (Throwable error) {
            log(ATHENA_TAG, "Cannot determine application readiness: " + error);
            return false;
        }
    }

    static void recordUnsupportedModernBroadcast(Throwable failure) {
        HookHealthRegistry.declareHook(MODERN_BROADCAST_HOOK_ID, false);
        HookHealthRegistry.recordRegistrationFailure(MODERN_BROADCAST_HOOK_ID, failure);
    }

    public static void hookAndroid(ClassLoader classLoader) {
        log("Freezeit[Xposed]", BuildConfig.VERSION_NAME);

        Config config = new Config();

        new FreezeitService(config, classLoader);
        new AlarmHook(config, classLoader);
        new AnrHook(config, classLoader);
        if (Build.VERSION.SDK_INT >= ANDROID_16_API_LEVEL) {
            UnsupportedOperationException failure = new UnsupportedOperationException(
                    "BroadcastQueueModernImpl is not implemented");
            recordUnsupportedModernBroadcast(failure);
            log("Freezeit[Broadcast]", "Disabled legacy broadcast hooks: " + failure.getMessage());
        } else {
            new BroadCastHook(config, classLoader);
        }
        new WakeLockHook(config, classLoader); //FreezeitService 的 handleWakeLock 暂时不用
    }
}
