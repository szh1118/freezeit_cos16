package io.github.jark006.freezeit.hook.app;

import android.app.Application;
import android.content.Context;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;

import java.io.FileInputStream;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.Callable;

import io.github.jark006.freezeit.hook.AthenaSignatureTable;
import io.github.jark006.freezeit.hook.Enum;
import io.github.jark006.freezeit.hook.HookHealthRegistry;
import io.github.jark006.freezeit.hook.XpUtils;

// ColorOS/Athena package-level background cleanup mitigation.
public class OplusAthena {
    private static final String TAG = "Freezeit[OplusAthena]:";
    private static final String SIGNATURE_HOOK_ID = "athena#signature_selection";
    private static final String INSTRUMENTATION_CLASS = "android.app.Instrumentation";
    private static final String CALL_APPLICATION_ON_CREATE = "callApplicationOnCreate";
    private static final int MAX_LOG_VALUE_LENGTH = 96;
    private static final Object INSTALL_LOCK = new Object();

    private static boolean athenaHooksInstalled;
    private static boolean lifecycleHookRegistrationStarted;

    public static void Hook(ClassLoader classLoader) {
        HookHealthRegistry.declareHook(SIGNATURE_HOOK_ID, true);
        Application application = AthenaIdentity.currentApplication();
        if (application == null) {
            registerApplicationLifecycleHook(classLoader);
            return;
        }

        installAthenaHooks(classLoader, application);
    }

    private static void registerApplicationLifecycleHook(final ClassLoader classLoader) {
        synchronized (INSTALL_LOCK) {
            if (athenaHooksInstalled || lifecycleHookRegistrationStarted) return;
            lifecycleHookRegistrationStarted = true;
        }

        boolean registered = hook(true, classLoader, new XpUtils.MethodHook() {
                    @Override
                    protected void beforeHookedMethod(XpUtils.MethodHookParam param) {
                        if (param.args == null || param.args.length == 0
                                || !(param.args[0] instanceof Application)) {
                            XpUtils.log(TAG, "Application lifecycle callback has no Application argument");
                            return;
                        }
                        installAthenaHooks(classLoader, (Application) param.args[0]);
                    }
                }, INSTRUMENTATION_CLASS, CALL_APPLICATION_ON_CREATE, Application.class);
        if (!registered) {
            synchronized (INSTALL_LOCK) {
                lifecycleHookRegistrationStarted = false;
            }
            HookHealthRegistry.recordRegistrationFailure(SIGNATURE_HOOK_ID,
                    new IllegalStateException("Cannot register Instrumentation.callApplicationOnCreate"));
        }
    }

    private static void installAthenaHooks(ClassLoader classLoader, Application application) {
        synchronized (INSTALL_LOCK) {
            if (athenaHooksInstalled) return;

            AthenaIdentity identity = AthenaIdentity.read(application);
            AthenaSignatureTable.Selection selection = AthenaSignatureTable.select(
                    identity.versionName, identity.apkSha256, identity.romFingerprint);
            if (!selection.isHookingAllowed()) {
                IllegalStateException failure = new IllegalStateException(selection.getReason());
                HookHealthRegistry.recordRegistrationFailure(SIGNATURE_HOOK_ID, failure);
                XpUtils.log(TAG, "Fail-closed: " + selection.getReason());
                return;
            }
            HookHealthRegistry.recordRegistered(SIGNATURE_HOOK_ID);
            athenaHooksInstalled = true;
            installHookRegistrations(classLoader, selection.getSignatureSet());
        }
    }

    private static void installHookRegistrations(ClassLoader classLoader,
                                                 AthenaSignatureTable.SignatureSet signatures) {

        hookExternalClearStrategy(classLoader, signatures, Enum.Class.OplusForceStopStrategy);
        hookExternalClearStrategy(classLoader, signatures, Enum.Class.OplusKillPidStrategy);
        hookExternalClearStrategy(classLoader, signatures, Enum.Class.OplusKillUidStrategy);
        hook(true, classLoader, blockVoid("external clear ForceStopOrKillStrategy"),
                Enum.Class.OplusForceStopOrKillStrategy, Enum.Method.oplusForceStopOrKill,
                signatures.getForceStopOrKillValueClass(), Enum.Class.OplusClearRecord,
                Enum.Class.OplusForceStopStrategy + "$ForceStopItemResult");

        hook(true, classLoader, blockVoid("force-stop b"),
                signatures.getClearUtilsClass(), Enum.Method.oplusForceStop,
                Context.class, String.class, int.class, int.class, int.class, String.class, String.class);
        hook(true, classLoader, blockVoid("force-stop c"),
                signatures.getClearUtilsClass(), Enum.Method.oplusForceStopWithFlag,
                Context.class, String.class, int.class, int.class, int.class, String.class, String.class, boolean.class);
        hook(true, classLoader, blockBoolean("kill d"),
                signatures.getClearUtilsClass(), Enum.Method.oplusKillSimple,
                int.class, int.class, String.class, int.class, int.class, int.class, String.class, String.class);
        hook(true, classLoader, blockBoolean("kill e"),
                signatures.getClearUtilsClass(), Enum.Method.oplusKill,
                int.class, int.class, String.class, int.class, int.class, int.class,
                String.class, String.class, Callable.class, Callable.class);
        hook(true, classLoader, blockVoid("clear action kill h"),
                signatures.getClearActionClass(), Enum.Method.oplusClearActionKill,
                int.class, int.class, String.class, int.class, int.class, int.class,
                String.class, String.class, String.class);

        hook(false, classLoader, logOnly("GuardElf policy"),
                Enum.Class.OplusRemoteGuardElfServiceStub, Enum.Method.onPowerProtectPolicyChange,
                String.class, int.class);
        hook(false, classLoader, logOnly("GuardElf switch"),
                Enum.Class.OplusRemoteGuardElfServiceStub, Enum.Method.setGuardElfSwitch,
                boolean.class, String.class);
    }

    private static void hookExternalClearStrategy(ClassLoader classLoader,
                                                  AthenaSignatureTable.SignatureSet signatures,
                                                  String className) {
        hook(true, classLoader, blockList("external clear " + simpleName(className)),
                className, Enum.Method.oplusExternalClear,
                List.class,
                signatures.getExternalClearRequestClass(),
                signatures.getExternalClearContextClass(),
                Enum.Class.OplusClearRecord,
                Enum.Class.OplusKeepRecord);
    }

    private static boolean hook(boolean critical, ClassLoader classLoader, XpUtils.MethodHook callback,
                                String className, String methodName, Object... parameterTypes) {
        String hookId = hookId(className, methodName, parameterTypes);
        HookHealthRegistry.declareHook(hookId, critical);
        return XpUtils.hookMethod(TAG, classLoader, callback, className, methodName, parameterTypes);
    }

    private static String hookId(String className, String methodName, Object[] parameterTypes) {
        StringBuilder builder = new StringBuilder(className).append('#').append(methodName).append('(');
        for (int index = 0; index < parameterTypes.length; index++) {
            if (index > 0) builder.append(',');
            Object parameterType = parameterTypes[index];
            builder.append(parameterType instanceof Class
                    ? ((Class<?>) parameterType).getName() : String.valueOf(parameterType));
        }
        return builder.append(')').toString();
    }

    private static XpUtils.MethodHook blockList(final String target) {
        return new XpUtils.MethodHook() {
            @Override
            protected void beforeHookedMethod(XpUtils.MethodHookParam param) {
                XpUtils.log(TAG, "Blocked " + target + " " + describeArgs(param.args));
                param.setResult(new ArrayList<>());
            }
        };
    }

    private static XpUtils.MethodHook blockVoid(final String target) {
        return new XpUtils.MethodHook() {
            @Override
            protected void beforeHookedMethod(XpUtils.MethodHookParam param) {
                XpUtils.log(TAG, "Blocked " + target + " " + describeArgs(param.args));
                param.setResult(null);
            }
        };
    }

    private static XpUtils.MethodHook blockBoolean(final String target) {
        return new XpUtils.MethodHook() {
            @Override
            protected void beforeHookedMethod(XpUtils.MethodHookParam param) {
                XpUtils.log(TAG, "Blocked " + target + " " + describeArgs(param.args));
                param.setResult(false);
            }
        };
    }

    private static XpUtils.MethodHook logOnly(final String target) {
        return new XpUtils.MethodHook() {
            @Override
            protected void beforeHookedMethod(XpUtils.MethodHookParam param) {
                XpUtils.log(TAG, target + " " + describeArgs(param.args));
            }
        };
    }

    private static String describeArgs(Object[] args) {
        if (args == null || args.length == 0) return "";
        StringBuilder builder = new StringBuilder("[");
        for (int i = 0; i < args.length; i++) {
            if (i > 0) builder.append(", ");
            builder.append(describe(args[i]));
        }
        return builder.append(']').toString();
    }

    private static String describe(Object arg) {
        if (arg == null) return "null";
        if (arg instanceof List<?>) {
            return "List(size=" + ((List<?>) arg).size() + ')';
        }
        if (arg instanceof CharSequence) {
            CharSequence value = (CharSequence) arg;
            int length = value.length();
            int prefixLength = Math.min(length, MAX_LOG_VALUE_LENGTH);
            String prefix = value.subSequence(0, prefixLength).toString();
            return length > prefixLength ? prefix + "..." : prefix;
        }
        String value = String.valueOf(arg);
        if (value.length() > MAX_LOG_VALUE_LENGTH) {
            value = value.substring(0, MAX_LOG_VALUE_LENGTH) + "...";
        }
        return value;
    }

    private static String simpleName(String className) {
        int index = className.lastIndexOf('.');
        return index >= 0 ? className.substring(index + 1) : className;
    }

    private static final class AthenaIdentity {
        private final String versionName;
        private final String apkSha256;
        private final String romFingerprint;

        private AthenaIdentity(String versionName, String apkSha256, String romFingerprint) {
            this.versionName = versionName;
            this.apkSha256 = apkSha256;
            this.romFingerprint = romFingerprint;
        }

        private static Application currentApplication() {
            try {
                return (Application) Class.forName("android.app.ActivityThread")
                        .getDeclaredMethod("currentApplication").invoke(null);
            } catch (Throwable error) {
                XpUtils.log(TAG, "Cannot resolve current Athena Application: " + error);
                return null;
            }
        }

        private static AthenaIdentity read(Application application) {
            String versionName = "unknown";
            String apkSha256 = "unknown";
            try {
                PackageInfo packageInfo = application.getPackageManager()
                        .getPackageInfo(Enum.Package.oplusAthena, 0);
                versionName = String.valueOf(packageInfo.versionName);
                ApplicationInfo applicationInfo = packageInfo.applicationInfo;
                if (applicationInfo != null) {
                    apkSha256 = sha256(applicationInfo.sourceDir);
                }
            } catch (Throwable error) {
                XpUtils.log(TAG, "Cannot resolve Athena APK identity: " + error);
            }
            return new AthenaIdentity(versionName, apkSha256,
                    systemProperty("ro.system_ext.build.fingerprint"));
        }

        private static String systemProperty(String name) {
            try {
                Class<?> properties = Class.forName("android.os.SystemProperties");
                return String.valueOf(properties.getDeclaredMethod("get", String.class, String.class)
                        .invoke(null, name, "unknown"));
            } catch (Throwable error) {
                XpUtils.log(TAG, "Cannot resolve ROM fingerprint: " + error);
                return "unknown";
            }
        }

        private static String sha256(String path) throws Exception {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            byte[] buffer = new byte[32 * 1024];
            try (FileInputStream inputStream = new FileInputStream(path)) {
                int count;
                while ((count = inputStream.read(buffer)) != -1) {
                    digest.update(buffer, 0, count);
                }
            }
            StringBuilder result = new StringBuilder(64);
            for (byte value : digest.digest()) {
                result.append(String.format("%02x", value & 0xff));
            }
            return result.toString();
        }
    }
}
