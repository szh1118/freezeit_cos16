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

    public static void Hook(ClassLoader classLoader) {
        HookHealthRegistry.declareHook(SIGNATURE_HOOK_ID, true);
        AthenaIdentity identity = AthenaIdentity.read();
        AthenaSignatureTable.Selection selection = AthenaSignatureTable.select(
                identity.versionName, identity.apkSha256, identity.romFingerprint);
        if (!selection.isHookingAllowed()) {
            IllegalStateException failure = new IllegalStateException(selection.getReason());
            HookHealthRegistry.recordRegistrationFailure(SIGNATURE_HOOK_ID, failure);
            XpUtils.log(TAG, "Fail-closed: " + selection.getReason());
            return;
        }
        HookHealthRegistry.recordRegistered(SIGNATURE_HOOK_ID);
        AthenaSignatureTable.SignatureSet signatures = selection.getSignatureSet();

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
        String value = String.valueOf(arg);
        if (value.length() > 96) {
            value = value.substring(0, 96) + "...";
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

        private static AthenaIdentity read() {
            String versionName = "unknown";
            String apkSha256 = "unknown";
            try {
                Class<?> activityThread = Class.forName("android.app.ActivityThread");
                Application application = (Application) activityThread
                        .getDeclaredMethod("currentApplication").invoke(null);
                if (application != null) {
                    PackageInfo packageInfo = application.getPackageManager()
                            .getPackageInfo(Enum.Package.oplusAthena, 0);
                    versionName = String.valueOf(packageInfo.versionName);
                    ApplicationInfo applicationInfo = packageInfo.applicationInfo;
                    if (applicationInfo != null) {
                        apkSha256 = sha256(applicationInfo.sourceDir);
                    }
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
