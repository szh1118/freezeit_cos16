package io.github.jark006.freezeit.hook;

import android.annotation.SuppressLint;

import androidx.annotation.NonNull;

import java.lang.reflect.Field;
import java.util.HashMap;

import io.github.jark006.freezeit.hook.XpUtils.BucketSet;
import io.github.jark006.freezeit.hook.XpUtils.VectorSet;

public class Config {

    boolean mEnableModernQueue = false; // Android14+ SDK34+ 新广播机制

    public int[] settings = new int[256];
    public BucketSet managedApp = new BucketSet();// 受冻它管控的应用 只含冻结配置和杀死后台 不含自由后台
    public BucketSet permissive = new BucketSet();  // 宽松前台
    public VectorSet foregroundUid = new VectorSet(64); // 当前在前台(含宽松前台) 底层进程问询时才刷新
    public VectorSet pendingUid = new VectorSet(64);    // 切到后台暂未冻结的应用
    public HashMap<String, Integer> uidIndex = new LockedHashMap<>(512); // UID索引
    public HashMap<Integer, String> pkgIndex = new LockedHashMap<>(512); // 包名索引

    Field processRecordUidField,
            mCurProcStateField,
            broadcastFilterOwningUidField,
            broadcastRecordCallingUidField,
            broadcastRecordDeliveryField,
            serviceRecordDefiningUidField,
            alarmUidField,
            processRecordStateField,
            mScreenStateField;

    public volatile boolean initField = false;

    public final boolean isCurProcStateInitialized() {
        return initField && mCurProcStateField != null && processRecordStateField != null;
    }

    @SuppressLint("PrivateApi")
    public synchronized String Init(ClassLoader classLoader) {
        if (initField)
            return "[SUCCESS]";

        initField = false;
        clearFields();

        Field nextProcessRecordUidField = null;
        Field nextCurProcStateField = null;
        Field nextBroadcastFilterOwningUidField = null;
        Field nextBroadcastRecordCallingUidField = null;
        Field nextBroadcastRecordDeliveryField = null;
        Field nextServiceRecordDefiningUidField = null;
        Field nextAlarmUidField = null;
        Field nextProcessRecordStateField = null;
        Field nextScreenStateField = null;

        try {
            // 需进入桌面后才能初始化
            nextCurProcStateField = Class.forName(Enum.Class.ProcessStateRecord, true, classLoader).getDeclaredField(Enum.Field.mCurProcState);
            nextProcessRecordUidField = Class.forName(Enum.Class.ProcessRecord, true, classLoader).getDeclaredField(Enum.Field.uid);
            nextBroadcastFilterOwningUidField = Class.forName(Enum.Class.BroadcastFilter, true, classLoader).getDeclaredField(Enum.Field.owningUid);
            nextBroadcastRecordCallingUidField = Class.forName(Enum.Class.BroadcastRecord, true, classLoader).getDeclaredField(Enum.Field.callingUid);
            nextBroadcastRecordDeliveryField = Class.forName(Enum.Class.BroadcastRecord, true, classLoader).getDeclaredField(Enum.Field.delivery);
            nextServiceRecordDefiningUidField = Class.forName(Enum.Class.ServiceRecord, true, classLoader).getDeclaredField(Enum.Field.definingUid);
            nextAlarmUidField = Class.forName(Enum.Class.AlarmS, true, classLoader).getDeclaredField(Enum.Field.uid);
            nextProcessRecordStateField = Class.forName(Enum.Class.ProcessRecord, true, classLoader).getDeclaredField(Enum.Field.mState);
            nextScreenStateField = Class.forName(Enum.Class.DisplayPowerState, true, classLoader).getDeclaredField(Enum.Field.mScreenState);

            nextCurProcStateField.setAccessible(true);
            nextProcessRecordUidField.setAccessible(true);
            nextBroadcastFilterOwningUidField.setAccessible(true);
            nextBroadcastRecordCallingUidField.setAccessible(true);
            nextBroadcastRecordDeliveryField.setAccessible(true);
            nextServiceRecordDefiningUidField.setAccessible(true);
            nextAlarmUidField.setAccessible(true);
            nextProcessRecordStateField.setAccessible(true);
            nextScreenStateField.setAccessible(true);

            mCurProcStateField = nextCurProcStateField;
            processRecordUidField = nextProcessRecordUidField;
            broadcastFilterOwningUidField = nextBroadcastFilterOwningUidField;
            broadcastRecordCallingUidField = nextBroadcastRecordCallingUidField;
            broadcastRecordDeliveryField = nextBroadcastRecordDeliveryField;
            serviceRecordDefiningUidField = nextServiceRecordDefiningUidField;
            alarmUidField = nextAlarmUidField;
            processRecordStateField = nextProcessRecordStateField;
            mScreenStateField = nextScreenStateField;

            initField = true;
            return "[SUCCESS]";
        } catch (Throwable error) {
            if (error instanceof VirtualMachineError)
                throw (VirtualMachineError) error;

            clearFields();
            initField = false;

            return "\n[ !!! FAIL !!! ]\n[ !!! 失败 !!! ]\n[ !!! FAIL !!! ]\n" +
                    (nextCurProcStateField != null ? 'O' : 'X') +
                    (nextProcessRecordUidField != null ? 'O' : 'X') +
                    (nextBroadcastFilterOwningUidField != null ? 'O' : 'X') +
                    (nextBroadcastRecordCallingUidField != null ? 'O' : 'X') +
                    (nextBroadcastRecordDeliveryField != null ? 'O' : 'X') +
                    (nextServiceRecordDefiningUidField != null ? 'O' : 'X') +
                    (nextAlarmUidField != null ? 'O' : 'X') +
                    (nextProcessRecordStateField != null ? 'O' : 'X') +
                    (nextScreenStateField != null ? 'O' : 'X') +
                    error;
        }
    }

    private void clearFields() {
        processRecordUidField = null;
        mCurProcStateField = null;
        broadcastFilterOwningUidField = null;
        broadcastRecordCallingUidField = null;
        broadcastRecordDeliveryField = null;
        serviceRecordDefiningUidField = null;
        alarmUidField = null;
        processRecordStateField = null;
        mScreenStateField = null;
    }

    public final int getProcessRecordUid(@NonNull Object obj) {
        try {
            return !initField || processRecordUidField == null ? -1 : processRecordUidField.getInt(obj);
        } catch (Exception e) {
            return -1;
        }
    }

    public final Object getProcessRecordState(@NonNull Object obj) {
        try {
            return !initField || processRecordStateField == null ? null : processRecordStateField.get(obj);
        } catch (Exception e) {
            return null;
        }
    }

    public final int getCurProcState(@NonNull Object obj) {
        try {
            return !initField || mCurProcStateField == null ? -1 : mCurProcStateField.getInt(obj);
        } catch (Exception e) {
            return -1;
        }
    }

    public final int getBroadcastFilterOwningUid(@NonNull Object obj) {
        try {
            return !initField || broadcastFilterOwningUidField == null ? -1 : broadcastFilterOwningUidField.getInt(obj);
        } catch (Exception e) {
            return -1;
        }
    }

    public final int getBroadcastRecordCallingUid(@NonNull Object obj) {
        try {
            return !initField || broadcastRecordCallingUidField == null ? -1 : broadcastRecordCallingUidField.getInt(obj);
        } catch (Exception e) {
            return -1;
        }
    }

    public final int[] getBroadcastRecordDelivery(@NonNull Object obj) {
        try {
            return !initField || broadcastRecordDeliveryField == null ? null : (int[]) broadcastRecordDeliveryField.get(obj);
        } catch (Exception e) {
            return null;
        }
    }

    public final int getServiceRecordDefiningUid(@NonNull Object obj) {
        try {
            return !initField || serviceRecordDefiningUidField == null ? -1 : serviceRecordDefiningUidField.getInt(obj);
        } catch (Exception e) {
            return -1;
        }
    }

    public final int getAlarmUid(@NonNull Object obj) {
        try {
            return !initField || alarmUidField == null ? -1 : alarmUidField.getInt(obj);
        } catch (Exception e) {
            return -1;
        }
    }

    public final int getScreenState(@NonNull Object obj) {
        try {
            return !initField || mScreenStateField == null ? 0 : mScreenStateField.getInt(obj);
        } catch (Exception e) {
            return 0;
        }
    }

    private static final class LockedHashMap<K, V> extends HashMap<K, V> {
        LockedHashMap(int initialCapacity) {
            super(initialCapacity);
        }

        @Override
        public synchronized V put(K key, V value) {
            return super.put(key, value);
        }

        @Override
        public synchronized V get(Object key) {
            return super.get(key);
        }

        @Override
        public synchronized V getOrDefault(Object key, V defaultValue) {
            return super.getOrDefault(key, defaultValue);
        }

        @Override
        public synchronized boolean containsKey(Object key) {
            return super.containsKey(key);
        }

        @Override
        public synchronized V remove(Object key) {
            return super.remove(key);
        }

        @Override
        public synchronized void clear() {
            super.clear();
        }

        @Override
        public synchronized int size() {
            return super.size();
        }

        @Override
        public synchronized boolean isEmpty() {
            return super.isEmpty();
        }
    }

}
