package io.github.jark006.freezeit.hook;

import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.Arrays;
import java.util.Collections;
import java.util.HashSet;
import java.util.Set;

import io.github.jark006.freezeit.Utils;

public class XpUtils {
    public final static boolean DEBUG_WAKEUP_LOCK = true;
    public final static boolean DEBUG_BROADCAST_STATIC = true;
    public final static boolean DEBUG_BROADCAST_DYNAMIC = false;
    public final static boolean DEBUG_ALARM = true;
    public final static boolean DEBUG_ANR = true;
    public final static boolean DEBUG_PENDING_UID = false;

    static final int maxLogLength = 16000; // 16K 非KiB
    // Published builders are immutable after assignment, so direct socket reads see a whole log snapshot.
    public static volatile StringBuilder xpLogContent = new StringBuilder(maxLogLength);

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
        StringBuilder current = xpLogContent;
        StringBuilder next = new StringBuilder(maxLogLength);
        if (current.length() + TAG.length() + content.length() + 20 <= maxLogLength)
            next.append(current);

        var timeStamp = System.currentTimeMillis() / 1000 + 8 * 3600; //UTC+8
        var hour = (timeStamp / 3600) % 24;
        var min = (timeStamp % 3600) / 60;
        var sec = timeStamp % 60;

        if (hour < 10) next.append('0');
        next.append(hour).append(':');
        if (min < 10) next.append('0');
        next.append(min).append(':');
        if (sec < 10) next.append('0');
        next.append(sec).append(' ');

        next.append(TAG).append(": ").append(content).append('\n');
        xpLogContent = next;
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
        if (obj == null) {
            log("Freezeit[getObjectField]", "获取失败 null#" + fieldName);
            return null;
        }
        try {
            Field field = findField(obj.getClass(), fieldName);
            if (field == null) throw new NoSuchFieldException(fieldName);
            return field.get(obj);
        } catch (Exception e) {
            log("Freezeit[getObjectField]", "获取失败 " + getClassName(obj) + "#" + fieldName + ": " + e);
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
        if (obj == null) {
            log("Freezeit[getInt]", "获取失败 null#" + fieldName);
            return -1;
        }
        try {
            Field field = findField(obj.getClass(), fieldName);
            if (field == null) throw new NoSuchFieldException(fieldName);
            return field.getInt(obj);
        } catch (Exception e) {
            log("Freezeit[getInt]", "获取失败 " + getClassName(obj) + "#" + fieldName + ": " + e);
            return -1;
        }
    }

    public static boolean getBoolean(final Object obj, final String fieldName) {
        if (obj == null) {
            log("Freezeit[getBoolean]", "获取失败 null#" + fieldName);
            return false;
        }
        try {
            Field field = findField(obj.getClass(), fieldName);
            if (field == null) throw new NoSuchFieldException(fieldName);
            return field.getBoolean(obj);
        } catch (Exception e) {
            log("Freezeit[getBoolean]", "获取失败 " + getClassName(obj) + "#" + fieldName + ": " + e);
            return false;
        }
    }

    public static String getString(final Object obj, final String fieldName) {
        if (obj == null) {
            log("Freezeit[getString]", "获取失败 null#" + fieldName);
            return "null";
        }
        try {
            Field field = findField(obj.getClass(), fieldName);
            if (field == null) throw new NoSuchFieldException(fieldName);
            return (String) field.get(obj);
        } catch (Exception e) {
            log("Freezeit[getString]", "获取失败 " + getClassName(obj) + "#" + fieldName + ": " + e);
            return "null";
        }
    }

    private static String getClassName(Object obj) {
        return obj == null ? "null" : obj.getClass().getName();
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

    // 小集合使用不可变数组快照，读侧无需竞争 hook 热路径上的锁。
    public static class VectorSet {
        private static final int[] EMPTY_VECTOR = new int[0];

        int maxSize;
        volatile int[] vector;

        public VectorSet(int maxSize) {
            this.maxSize = Math.max(0, maxSize);
            vector = EMPTY_VECTOR;
        }

        public int size() {
            return vector.length;
        }

        public boolean isEmpty() {
            return vector.length == 0;
        }

        public synchronized void clear() {
            vector = EMPTY_VECTOR;
        }

        public synchronized void add(final int n) {
            int[] current = vector;
            for (int uid : current) {
                if (uid == n) return;
            }

            int[] next = Arrays.copyOf(current, current.length + 1);
            next[current.length] = n;
            vector = next;
            if (next.length > maxSize)
                maxSize = next.length;
        }

        public synchronized void erase(final int n) {
            int[] current = vector;
            for (int i = 0; i < current.length; i++) {
                if (current[i] == n) {
                    int[] next = new int[current.length - 1];
                    System.arraycopy(current, 0, next, 0, i);
                    if (i < next.length)
                        next[i] = current[current.length - 1];
                    vector = next;
                    return;
                }
            }
        }

        // 顺序查找
        public boolean contains(final int n) {
            if (n < 10000) return false;
            int[] current = vector;
            for (int uid : current) {
                if (uid == n)
                    return true;
            }
            return false;
        }

        public void toBytes(byte[] bytes, int byteOffset) {
            int[] current = vector;
            if (current.length > 0)
                Utils.Int2Byte(current, 0, current.length, bytes, byteOffset);
        }
    }

    // 应用 UID 可跨用户空间，不能假设局限在一个 4K 的主用户区间。
    public static class BucketSet {
        final int uidMin = 10000;
        private volatile Set<Integer> values;

        public BucketSet() {
            values = Collections.emptySet();
        }

        public int size() {
            return values.size();
        }

        public boolean isEmpty() {
            return values.isEmpty();
        }

        public synchronized void clear() {
            values = Collections.emptySet();
        }

        public synchronized void add(final int n) {
            if (n < uidMin)
                return;

            Set<Integer> current = values;
            if (current.contains(n)) return;

            Set<Integer> next = new HashSet<>(current);
            next.add(n);
            values = Collections.unmodifiableSet(next);
        }

        public synchronized void erase(final int n) {
            if (n < uidMin)
                return;

            Set<Integer> current = values;
            if (!current.contains(n)) return;

            Set<Integer> next = new HashSet<>(current);
            next.remove(n);
            values = next.isEmpty() ? Collections.emptySet() : Collections.unmodifiableSet(next);
        }

        public boolean contains(final int n) {
            return n >= uidMin && values.contains(n);
        }
    }
}
