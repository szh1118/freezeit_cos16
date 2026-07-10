package io.github.jark006.freezeit.hook;

import java.lang.reflect.Constructor;
import java.lang.reflect.Method;

import de.robv.android.xposed.XC_MethodHook;
import de.robv.android.xposed.XposedBridge;
import de.robv.android.xposed.XposedHelpers;

public class LegacyXposedBackend implements XpUtils.HookBackend {
    @Override
    public boolean hookMethod(String TAG, ClassLoader classLoader, XpUtils.MethodHook callback,
                              String className, String methodName, Object... parameterTypes) {
        String hookId = hookId(className, methodName, parameterTypes);
        Class<?> clazz;
        try {
            clazz = XposedHelpers.findClass(className, classLoader);
            HookHealthRegistry.recordClassResolved(hookId);
        } catch (Throwable error) {
            HookHealthRegistry.recordClassResolutionFailure(hookId, error);
            XpUtils.log(TAG, "Cannot hookMethod: " + methodName + ", cannot find " + className);
            return false;
        }
        Method method;
        try {
            method = XposedHelpers.findMethodExactIfExists(clazz, methodName, parameterTypes);
        } catch (Throwable error) {
            HookHealthRegistry.recordMethodMatchFailure(hookId, error);
            XpUtils.log(TAG, "Cannot hookMethod: " + methodName + " (" + error + ")");
            return false;
        }
        if (method == null) {
            HookHealthRegistry.recordMethodMatchFailure(hookId,
                    new NoSuchMethodException(className + "#" + methodName));
            XpUtils.log(TAG, "Cannot hookMethod: " + methodName);
            return false;
        }
        HookHealthRegistry.recordMethodMatched(hookId);
        try {
            XposedBridge.hookMethod(method, adapt(callback, hookId));
            HookHealthRegistry.recordRegistered(hookId);
            XpUtils.log(TAG, "Success hookMethod: " + methodName);
            return true;
        } catch (Throwable error) {
            HookHealthRegistry.recordRegistrationFailure(hookId, error);
            XpUtils.log(TAG, "Cannot register hookMethod: " + hookId + " (" + error + ")");
            return false;
        }
    }

    @Override
    public void hookConstructor(String TAG, ClassLoader classLoader, XpUtils.MethodHook callback,
                                String className, Object... parameterTypes) {
        String hookId = hookId(className, "<init>", parameterTypes);
        Class<?> clazz;
        try {
            clazz = XposedHelpers.findClass(className, classLoader);
            HookHealthRegistry.recordClassResolved(hookId);
        } catch (Throwable error) {
            HookHealthRegistry.recordClassResolutionFailure(hookId, error);
            XpUtils.log(TAG, "Cannot hookConstructor, cannot find " + className);
            return;
        }
        Constructor<?> constructor;
        try {
            constructor = XposedHelpers.findConstructorExactIfExists(clazz, parameterTypes);
        } catch (Throwable error) {
            HookHealthRegistry.recordMethodMatchFailure(hookId, error);
            XpUtils.log(TAG, "Cannot hookConstructor: " + className + " (" + error + ")");
            return;
        }
        if (constructor == null) {
            HookHealthRegistry.recordMethodMatchFailure(hookId,
                    new NoSuchMethodException(className + "#<init>"));
            XpUtils.log(TAG, "Cannot hookConstructor: " + className);
            return;
        }
        HookHealthRegistry.recordMethodMatched(hookId);
        try {
            XposedBridge.hookMethod(constructor, adapt(callback, hookId));
            HookHealthRegistry.recordRegistered(hookId);
            XpUtils.log(TAG, "Success hookConstructor: " + className);
        } catch (Throwable error) {
            HookHealthRegistry.recordRegistrationFailure(hookId, error);
            XpUtils.log(TAG, "Cannot register hookConstructor: " + hookId + " (" + error + ")");
        }
    }

    private XC_MethodHook adapt(final XpUtils.MethodHook callback, final String hookId) {
        return new XC_MethodHook() {
            @Override
            protected void beforeHookedMethod(MethodHookParam param) throws Throwable {
                HookHealthRegistry.recordRuntimeInvocation(hookId);
                XpUtils.MethodHookParam freezeitParam =
                        new XpUtils.MethodHookParam(param.thisObject, param.args);
                callback.beforeHookedMethod(freezeitParam);
                if (freezeitParam.hasThrowable()) {
                    param.setThrowable(freezeitParam.getThrowable());
                } else if (freezeitParam.isReturnEarly()) {
                    param.setResult(freezeitParam.getResult());
                }
            }

            @Override
            protected void afterHookedMethod(MethodHookParam param) throws Throwable {
                XpUtils.MethodHookParam freezeitParam =
                        new XpUtils.MethodHookParam(param.thisObject, param.args);
                if (param.hasThrowable()) {
                    freezeitParam.setThrowable(param.getThrowable());
                } else {
                    freezeitParam.setProceedResult(param.getResult());
                }
                callback.afterHookedMethod(freezeitParam);
                if (freezeitParam.hasThrowable()) {
                    param.setThrowable(freezeitParam.getThrowable());
                } else if (freezeitParam.isReturnEarly()) {
                    param.setResult(freezeitParam.getResult());
                }
            }
        };
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
}
