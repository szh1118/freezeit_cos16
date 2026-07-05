package io.github.jark006.freezeit.hook;

import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.Arrays;

import io.github.jark006.freezeit.Utils;

public class XpUtils {
    public final static boolean DEBUG_WAKEUP_LOCK = true;
    public final static boolean DEBUG_BROADCAST_STATIC = true;
    public final static boolean DEBUG_BROADCAST_DYNAMIC = false;
    public final static boolean DEBUG_ALARM = true;
    public final static boolean DEBUG_ANR = true;
    public final static boolean DEBUG_PENDING_UID = false;

    static final int maxLogLength = 16000; // 16K 非KiB
    public static StringBuilder xpLogContent = new StringBuilder(maxLogLength); // StringBuffer?

    private static HookBackend hookBackend = new MissingHookBackend();

    public interface HookBackend {
        boolean hookMethod(String TAG, ClassLoader classLoader, MethodHook callback,
                           String className, String methodName, Object... parameterTypes);

        void hookConstructor(String TAG, ClassLoader classLoader, MethodHook callback,
                             String className, Object... parameterTypes);
    }

    public static class MethodHook {
        protected void beforeHookedMethod(MethodHookParam param) throws Throwable {
        }

        protected void afterHookedMethod(MethodHookParam param) throws Throwable {
        }
    }

    public static class MethodHookParam {
        public final Object thisObject;
        public final Object[] args;

        private Object result;
        private Throwable throwable;
        private boolean returnEarly;

        public MethodHookParam(Object thisObject, Object[] args) {
            this.thisObject = thisObject;
            this.args = args;
        }

        public Object getResult() {
            return result;
        }

        public void setResult(Object result) {
            this.result = result;
            this.throwable = null;
            this.returnEarly = true;
        }

        public void setThrowable(Throwable throwable) {
            this.throwable = throwable;
            this.result = null;
            this.returnEarly = true;
        }

        public Throwable getThrowable() {
            return throwable;
        }

        public boolean hasThrowable() {
            return throwable != null;
        }

        public boolean isReturnEarly() {
            return returnEarly;
        }

        public void setProceedResult(Object result) {
            this.result = result;
            this.throwable = null;
        }
    }

    public static final MethodHook DO_NOTHING = returnConstant(null);

    public static MethodHook returnConstant(final Object result) {
        return new MethodHook() {
            @Override
            protected void beforeHookedMethod(MethodHookParam param) {
                param.setResult(result);
            }
        };
    }

    public static synchronized void setHookBackend(HookBackend backend) {
        hookBackend = backend == null ? new MissingHookBackend() : backend;
    }

    public static synchronized void log(final String TAG, final String content) {
        if (xpLogContent.length() + TAG.length() + content.length() + 20 > maxLogLength)
            xpLogContent.setLength(0);

        var timeStamp = System.currentTimeMillis() / 1000 + 8 * 3600; //UTC+8
        var hour = (timeStamp / 3600) % 24;
        var min = (timeStamp % 3600) / 60;
        var sec = timeStamp % 60;

        if (hour < 10) xpLogContent.append('0');
        xpLogContent.append(hour).append(':');
        if (min < 10) xpLogContent.append('0');
        xpLogContent.append(min).append(':');
        if (sec < 10) xpLogContent.append('0');
        xpLogContent.append(sec).append(' ');

        xpLogContent.append(TAG).append(": ").append(content).append('\n');
    }

    public static boolean hookMethod(String TAG, ClassLoader classLoader, MethodHook callback,
                                     String className, String methodName, Object... parameterTypes) {
        return hookBackend.hookMethod(TAG, classLoader, callback, className, methodName, parameterTypes);
    }

    public static void hookConstructor(String TAG, ClassLoader classLoader, MethodHook callback,
                                       String className, Object... parameterTypes) {
        hookBackend.hookConstructor(TAG, classLoader, callback, className, parameterTypes);
    }

    public static Class<?> findClassIfExists(String className, ClassLoader classLoader) {
        try {
            return Class.forName(className, false, classLoader);
        } catch (Throwable ignored) {
            return null;
        }
    }

    public static Method findMethodExactIfExists(String className, ClassLoader classLoader,
                                                 String methodName, Object... parameterTypes) {
        Class<?> clazz = findClassIfExists(className, classLoader);
        return clazz == null ? null : findMethodExactIfExists(clazz, classLoader, methodName, parameterTypes);
    }

    public static Method findMethodExactIfExists(Class<?> clazz, ClassLoader classLoader,
                                                 String methodName, Object... parameterTypes) {
        try {
            Class<?>[] parameterClasses = resolveParameterTypes(classLoader, parameterTypes);
            Method method = clazz.getDeclaredMethod(methodName, parameterClasses);
            method.setAccessible(true);
            return method;
        } catch (Throwable ignored) {
            return null;
        }
    }

    public static Constructor<?> findConstructorExactIfExists(String className, ClassLoader classLoader,
                                                              Object... parameterTypes) {
        Class<?> clazz = findClassIfExists(className, classLoader);
        return clazz == null ? null : findConstructorExactIfExists(clazz, classLoader, parameterTypes);
    }

    public static Constructor<?> findConstructorExactIfExists(Class<?> clazz, ClassLoader classLoader,
                                                              Object... parameterTypes) {
        try {
            Class<?>[] parameterClasses = resolveParameterTypes(classLoader, parameterTypes);
            Constructor<?> constructor = clazz.getDeclaredConstructor(parameterClasses);
            constructor.setAccessible(true);
            return constructor;
        } catch (Throwable ignored) {
            return null;
        }
    }

    public static Class<?>[] resolveParameterTypes(ClassLoader classLoader, Object... parameterTypes)
            throws ClassNotFoundException {
        Class<?>[] result = new Class<?>[parameterTypes.length];
        for (int i = 0; i < parameterTypes.length; i++) {
            Object parameterType = parameterTypes[i];
            if (parameterType instanceof Class<?>) {
                result[i] = (Class<?>) parameterType;
            } else if (parameterType instanceof String) {
                result[i] = Class.forName((String) parameterType, false, classLoader);
            } else {
                throw new ClassNotFoundException("Unsupported parameter type: " + parameterType);
            }
        }
        return result;
    }

    public static Object getObjectField(final Object obj, final String fieldName) {
        try {
            Field field = findField(obj.getClass(), fieldName);
            if (field == null) throw new NoSuchFieldException(fieldName);
            return field.get(obj);
        } catch (Exception e) {
            log("Freezeit[getObjectField]", "获取失败 " + obj.getClass().getName() + "#" + fieldName + ": " + e);
            return null;
        }
    }

    public static Object newInstance(Class<?> clazz, Object... args) throws ReflectiveOperationException {
        Constructor<?> constructor = findCompatibleConstructor(clazz, args);
        if (constructor == null) throw new NoSuchMethodException(clazz.getName());
        return constructor.newInstance(args);
    }

    public static Object callMethod(Object obj, String methodName, Object... args) throws ReflectiveOperationException {
        Method method = findCompatibleMethod(obj.getClass(), methodName, args);
        if (method == null) throw new NoSuchMethodException(obj.getClass().getName() + "#" + methodName);
        return method.invoke(obj, args);
    }

    public static int getInt(final Object obj, final String fieldName) {
        try {
            Field field = findField(obj.getClass(), fieldName);
            if (field == null) throw new NoSuchFieldException(fieldName);
            return field.getInt(obj);
        } catch (Exception e) {
            log("Freezeit[getInt]", "获取失败 " + obj.getClass().getName() + "#" + fieldName + ": " + e);
            return -1;
        }
    }

    public static boolean getBoolean(final Object obj, final String fieldName) {
        try {
            Field field = findField(obj.getClass(), fieldName);
            if (field == null) throw new NoSuchFieldException(fieldName);
            return field.getBoolean(obj);
        } catch (Exception e) {
            log("Freezeit[getBoolean]", "获取失败 " + obj.getClass().getName() + "#" + fieldName + ": " + e);
            return false;
        }
    }

    public static String getString(final Object obj, final String fieldName) {
        try {
            Field field = findField(obj.getClass(), fieldName);
            if (field == null) throw new NoSuchFieldException(fieldName);
            return (String) field.get(obj);
        } catch (Exception e) {
            log("Freezeit[getString]", "获取失败 " + obj.getClass().getName() + "#" + fieldName + ": " + e);
            return "null";
        }
    }

    private static Field findField(Class<?> clazz, String fieldName) {
        Class<?> current = clazz;
        while (current != null) {
            try {
                Field field = current.getDeclaredField(fieldName);
                field.setAccessible(true);
                return field;
            } catch (NoSuchFieldException ignored) {
                current = current.getSuperclass();
            }
        }
        return null;
    }

    private static Constructor<?> findCompatibleConstructor(Class<?> clazz, Object[] args) {
        for (Constructor<?> constructor : clazz.getDeclaredConstructors()) {
            if (isCompatible(constructor.getParameterTypes(), args)) {
                constructor.setAccessible(true);
                return constructor;
            }
        }
        return null;
    }

    private static Method findCompatibleMethod(Class<?> clazz, String methodName, Object[] args) {
        Class<?> current = clazz;
        while (current != null) {
            for (Method method : current.getDeclaredMethods()) {
                if (method.getName().equals(methodName) && isCompatible(method.getParameterTypes(), args)) {
                    method.setAccessible(true);
                    return method;
                }
            }
            current = current.getSuperclass();
        }
        return null;
    }

    private static boolean isCompatible(Class<?>[] parameterTypes, Object[] args) {
        if (parameterTypes.length != args.length) return false;
        for (int i = 0; i < parameterTypes.length; i++) {
            Object arg = args[i];
            Class<?> parameterType = wrap(parameterTypes[i]);
            if (arg != null && !parameterType.isAssignableFrom(arg.getClass())) return false;
            if (arg == null && parameterTypes[i].isPrimitive()) return false;
        }
        return true;
    }

    private static Class<?> wrap(Class<?> type) {
        if (!type.isPrimitive()) return type;
        if (type == int.class) return Integer.class;
        if (type == long.class) return Long.class;
        if (type == boolean.class) return Boolean.class;
        if (type == byte.class) return Byte.class;
        if (type == short.class) return Short.class;
        if (type == char.class) return Character.class;
        if (type == float.class) return Float.class;
        if (type == double.class) return Double.class;
        if (type == void.class) return Void.class;
        return type;
    }

    private static class MissingHookBackend implements HookBackend {
        @Override
        public boolean hookMethod(String TAG, ClassLoader classLoader, MethodHook callback,
                                  String className, String methodName, Object... parameterTypes) {
            log(TAG, "Cannot hookMethod without backend: " + className + "#" + methodName);
            return false;
        }

        @Override
        public void hookConstructor(String TAG, ClassLoader classLoader, MethodHook callback,
                                    String className, Object... parameterTypes) {
            log(TAG, "Cannot hookConstructor without backend: " + className);
        }
    }

    // 少量元素(0-10)时，clear,add,contain 性能均优于 HashSet, TreeSet
    public static class VectorSet {
        int size = 0, maxSize;
        int[] vector;

        public VectorSet(int maxSize) {
            this.maxSize = maxSize;
            vector = new int[maxSize];
        }

        public int size() {
            return size;
        }

        public boolean isEmpty() {
            return size == 0;
        }

        public void clear() {
            size = 0;
        }

        public void add(final int n) {
            for (int i = 0; i < size; i++) {
                if (vector[i] == n) return;
            }
            if (size < maxSize)
                vector[size++] = n;
        }

        public void erase(final int n) {
            for (int i = 0; i < size; i++) {
                if (vector[i] == n) {
                    vector[i] = vector[--size];
                    return;
                }
            }
        }

        // 顺序查找
        public boolean contains(final int n) {
            if (n < 10000) return false;
            for (int i = 0; i < size; i++) {
                if (vector[i] == n)
                    return true;
            }
            return false;
        }

        public void toBytes(byte[] bytes, int byteOffset) {
            if (size > 0)
                Utils.Int2Byte(vector, 0, size, bytes, byteOffset);
        }
    }

    // 造轮子：常见UID位于 10000 ~ 14000
    // 在 APP UID 范围, 性能均优于HashSet
    public static class BucketSet {
        final int uidMin = 10000;
        final int uidMax = 14000;// 默认最多4千个应用
        int size = 0;
        boolean[] bucket = new boolean[uidMax - uidMin];

        public BucketSet() {
            clear();
        }

        public int size() {
            return size;
        }

        public boolean isEmpty() {
            return size == 0;
        }

        public void clear() {
            size = 0;
            Arrays.fill(bucket, false);
        }

        public void add(final int n) {
            if (n < uidMin || uidMax <= n)
                return;
            if (!bucket[n - uidMin]) {
                bucket[n - uidMin] = true;
                size++;
            }
        }

        public void erase(final int n) {
            if (n < uidMin || uidMax <= n)
                return;
            if (bucket[n - uidMin]) {
                bucket[n - uidMin] = false;
                size--;
            }
        }

        public boolean contains(final int n) {
            if (n < uidMin || uidMax <= n)
                return false;
            return bucket[n - uidMin];
        }
    }
}
