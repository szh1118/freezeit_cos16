package io.github.jark006.freezeit.hook;

import android.util.Log;

import java.lang.reflect.Constructor;
import java.lang.reflect.Executable;
import java.lang.reflect.Method;

import io.github.libxposed.api.XposedInterface;

public class ModernXposedBackend implements XpUtils.HookBackend {
    private static final String LOG_TAG = "Freezeit";

    private final XposedInterface xposed;

    public ModernXposedBackend(XposedInterface xposed) {
        this.xposed = xposed;
    }

    @Override
    public boolean hookMethod(String TAG, ClassLoader classLoader, XpUtils.MethodHook callback,
                              String className, String methodName, Object... parameterTypes) {
        String hookId = hookId(className, methodName, parameterTypes);
        try {
            Class<?> clazz = Class.forName(className, false, classLoader);
            HookHealthRegistry.recordClassResolved(hookId);
            Method method = XpUtils.findMethodExactIfExists(clazz, classLoader, methodName, parameterTypes);
            if (method == null) {
                HookHealthRegistry.recordMethodMatchFailure(hookId,
                        new NoSuchMethodException(className + "#" + methodName));
                XpUtils.log(TAG, "Cannot hookMethod: " + methodName);
                return false;
            }
            HookHealthRegistry.recordMethodMatched(hookId);
            hookExecutable(method, callback, hookId);
            HookHealthRegistry.recordRegistered(hookId);
            XpUtils.log(TAG, "Success hookMethod: " + methodName);
            return true;
        } catch (ClassNotFoundException | LinkageError error) {
            HookHealthRegistry.recordClassResolutionFailure(hookId, error);
            XpUtils.log(TAG, "Cannot hookMethod class: " + className + " (" + error + ")");
            return false;
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
        try {
            Class<?> clazz = Class.forName(className, false, classLoader);
            HookHealthRegistry.recordClassResolved(hookId);
            Constructor<?> constructor = XpUtils.findConstructorExactIfExists(className, classLoader, parameterTypes);
            if (constructor == null) {
                HookHealthRegistry.recordMethodMatchFailure(hookId,
                        new NoSuchMethodException(className + "#<init>"));
                XpUtils.log(TAG, "Cannot hookConstructor: " + className);
                return;
            }
            HookHealthRegistry.recordMethodMatched(hookId);
            hookExecutable(constructor, callback, hookId);
            HookHealthRegistry.recordRegistered(hookId);
            XpUtils.log(TAG, "Success hookConstructor: " + className);
        } catch (ClassNotFoundException | LinkageError error) {
            HookHealthRegistry.recordClassResolutionFailure(hookId, error);
            XpUtils.log(TAG, "Cannot hookConstructor class: " + className + " (" + error + ")");
        } catch (Throwable error) {
            HookHealthRegistry.recordRegistrationFailure(hookId, error);
            XpUtils.log(TAG, "Cannot register hookConstructor: " + hookId + " (" + error + ")");
        }
    }

    private void hookExecutable(Executable executable, XpUtils.MethodHook callback, String hookId) {
        xposed.hook(executable)
                .setExceptionMode(XposedInterface.ExceptionMode.PROTECTIVE)
                .intercept(chain -> intercept(callback, chain, hookId));
    }

    private Object intercept(XpUtils.MethodHook callback, XposedInterface.Chain chain,
                             String hookId) throws Throwable {
        HookHealthRegistry.recordRuntimeInvocation(hookId);
        Object[] args = chain.getArgs().toArray(new Object[0]);
        XpUtils.MethodHookParam param = new XpUtils.MethodHookParam(chain.getThisObject(), args);

        try {
            callback.beforeHookedMethod(param);
        } catch (Throwable error) {
            HookHealthRegistry.recordRuntimeFailure(hookId, error);
            throw error;
        }

        if (!param.isReturnEarly()) {
            try {
                param.setProceedResult(chain.proceed(param.args));
            } catch (Throwable error) {
                param.setThrowable(error);
            }
        }

        try {
            callback.afterHookedMethod(param);
        } catch (Throwable error) {
            HookHealthRegistry.recordRuntimeFailure(hookId, error);
            throw error;
        }
        if (param.hasThrowable()) throw param.getThrowable();
        return param.getResult();
    }

    private static String hookId(String className, String methodName, Object[] parameterTypes) {
        StringBuilder builder = new StringBuilder(className).append('#').append(methodName).append('(');
        for (int i = 0; i < parameterTypes.length; i++) {
            if (i > 0) builder.append(',');
            Object parameterType = parameterTypes[i];
            builder.append(parameterType instanceof Class
                    ? ((Class<?>) parameterType).getName() : String.valueOf(parameterType));
        }
        return builder.append(')').toString();
    }

    public void logFramework(String message) {
        try {
            xposed.log(Log.INFO, LOG_TAG, message);
        } catch (Throwable ignored) {
        }
    }
}
