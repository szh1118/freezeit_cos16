package io.github.jark006.freezeit.hook.android;

import static io.github.jark006.freezeit.hook.XpUtils.log;

import android.content.Context;
import android.net.Credentials;
import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.os.Build;
import android.os.Handler;
import android.os.SystemClock;

import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.lang.reflect.Array;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.Set;

import io.github.jark006.freezeit.Utils;
import io.github.jark006.freezeit.hook.Config;
import io.github.jark006.freezeit.hook.Enum;
import io.github.jark006.freezeit.hook.XpUtils;
import io.github.jark006.freezeit.hook.HookHealthRegistry;
import io.github.jark006.freezeit.hook.ScopedHealthReport;
import io.github.jark006.freezeit.hook.XpUtils.BucketSet;
import io.github.jark006.freezeit.hook.XpUtils.MethodHook;
import io.github.jark006.freezeit.hook.XpUtils.MethodHookParam;
import io.github.jark006.freezeit.hook.XpUtils.VectorSet;

public class FreezeitService {
    final static String TAG = "[Service]";

    final static String CFG_TAG = "[Config]";
    final static String AMS_TAG = "[AMS]";
    final static String NMS_TAG = "[Netd]";
    final static String WAK_TAG = "[AppOps]";
    final static String DPC_TAG = "[Display]";
    final static String FGD_TAG = "[Foreground]";
    final static String PED_TAG = "[Pending]";

    private static final int ROOT_UID = 0;
    private static final long RUNTIME_SNAPSHOT_MAX_AGE_MS = 15_000L;
    private static final long SOCKET_FRAME_TIMEOUT_MS = 3_000L;
    private static volatile RuntimeSnapshot runtimeSnapshot = RuntimeSnapshot.empty(new BucketSet());

    final int REPLY_SUCCESS = 2;
    final int REPLY_FAILURE = 0;

    Config config;

    private volatile ArrayList<?> mLruProcesses;
    private volatile Object mProcLock;

    Object mPowerState;

    Object appOpsService;
    Method setUidModeMethod;

    Object mNetdService;
    Class<?> UidRangeParcelClazz;

    LocalSocketServer serverThread = new LocalSocketServer();

    ClassLoader classLoader;

    static boolean shouldSuppressBackgroundWork(Config config, int uid) {
        RuntimeSnapshot snapshot = runtimeSnapshot;
        if (!snapshot.managedApps.contains(uid) || snapshot.foregroundUids.contains(uid) ||
                snapshot.pendingUids.contains(uid)) {
            return false;
        }

        final long now = SystemClock.elapsedRealtime();
        return isFreshSnapshot(snapshot.foregroundAtMs, now) &&
                isFreshSnapshot(snapshot.pendingAtMs, now);
    }

    private static boolean isFreshSnapshot(long snapshotAtMs, long nowMs) {
        return snapshotAtMs >= 0 && nowMs >= snapshotAtMs &&
                nowMs - snapshotAtMs <= RUNTIME_SNAPSHOT_MAX_AGE_MS;
    }

    private static final class RuntimeSnapshot {
        final BucketSet managedApps;
        final VectorSet foregroundUids;
        final VectorSet pendingUids;
        final long foregroundAtMs;
        final long pendingAtMs;

        RuntimeSnapshot(BucketSet managedApps, VectorSet foregroundUids, VectorSet pendingUids,
                        long foregroundAtMs, long pendingAtMs) {
            this.managedApps = managedApps;
            this.foregroundUids = foregroundUids;
            this.pendingUids = pendingUids;
            this.foregroundAtMs = foregroundAtMs;
            this.pendingAtMs = pendingAtMs;
        }

        static RuntimeSnapshot empty(BucketSet managedApps) {
            return new RuntimeSnapshot(managedApps, new VectorSet(64), new VectorSet(64), -1L, -1L);
        }
    }

    public FreezeitService(Config config, ClassLoader classLoader) {
        this.config = config;
        this.classLoader = classLoader;

        // A10-13 ActivityManagerService
        // https://cs.android.com/android/platform/superproject/main/+/main:frameworks/base/services/core/java/com/android/server/am/ActivityManagerService.java
        XpUtils.hookConstructor(AMS_TAG, classLoader, new MethodHook() {
            @Override
            protected void afterHookedMethod(MethodHookParam param) {
                Object mProcessList = XpUtils.getObjectField(param.thisObject, Enum.Field.mProcessList);
                mLruProcesses = mProcessList == null ? null :
                        (ArrayList<?>) XpUtils.getObjectField(mProcessList, Enum.Field.mLruProcesses);
                mProcLock = XpUtils.getObjectField(param.thisObject, "mProcLock");
                log(AMS_TAG, ((mLruProcesses == null || mProcLock == null ? "!!! Fail" : "Success")) +
                        " init mLruProcesses/mProcLock");
            }
        }, Enum.Class.ActivityManagerService, Context.class, Enum.Class.ActivityTaskManagerService);


        // A10-A13 NetworkManagementService
        // https://cs.android.com/android/platform/superproject/+/android-13.0.0_r74:frameworks/base/services/core/java/com/android/server/NetworkManagementService.java

        // latest
        // https://cs.android.com/android/platform/superproject/main/+/main:frameworks/base/services/core/java/com/android/server/NetworkManagementService.java
        UidRangeParcelClazz = XpUtils.findClassIfExists(Enum.Class.UidRangeParcel, classLoader);
        log(NMS_TAG, ((UidRangeParcelClazz == null ? "!!! Fail" : "Success")) + " init UidRangeParcel");
        XpUtils.hookMethod(NMS_TAG, classLoader, new MethodHook() {
                    @Override
                    protected void afterHookedMethod(MethodHookParam param) {
                        mNetdService = XpUtils.getObjectField(param.thisObject, Enum.Field.mNetdService);
                        log(NMS_TAG, ((mNetdService == null ? "!!! Fail" : "Success")) + " init mNetdService");
                    }
                },
                Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE ?
                        Enum.Class.NetworkManagementServiceU : Enum.Class.NetworkManagementService,
                Enum.Method.connectNativeNetdService);


        // AppOpsService
        // https://cs.android.com/android/platform/superproject/main/+/main:frameworks/base/services/core/java/com/android/server/appop/AppOpsService.java;l=1776
        setUidModeMethod = XpUtils.findMethodExactIfExists(
                Enum.Class.AppOpsService, classLoader, Enum.Method.setUidMode,
                int.class, int.class, int.class);
        log(WAK_TAG, ((setUidModeMethod == null ? "!!! Fail" : "Success")) + " init setUidModeMethod");

        MethodHook AppOpsHook = new MethodHook() {
            @Override
            protected void afterHookedMethod(MethodHookParam param) {
                appOpsService = param.thisObject;
                log(WAK_TAG, ((appOpsService == null ? "!!! Fail" : "Success")) + " init appOpsService");
            }
        };

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
            XpUtils.hookConstructor(WAK_TAG, classLoader, AppOpsHook, Enum.Class.AppOpsService,
                    File.class, File.class, Handler.class, Context.class);
        else // A11-13
            XpUtils.hookConstructor(WAK_TAG, classLoader, AppOpsHook, Enum.Class.AppOpsService,
                    File.class, Handler.class, Context.class);


        // A10-A13 DisplayPowerController
        // https://cs.android.com/android/platform/superproject/+/android-13.0.0_r74:frameworks/base/services/core/java/com/android/server/display/DisplayPowerState.java;l=145
        XpUtils.hookMethod(DPC_TAG, classLoader, new MethodHook() {
            @Override
            protected void afterHookedMethod(MethodHookParam param) {
                mPowerState = XpUtils.getObjectField(param.thisObject, Enum.Field.mPowerState);
                log(WAK_TAG, ((mPowerState == null ? "!!! Fail" : "Success")) + " init mPowerState");
            }
        }, Enum.Class.DisplayPowerController, Enum.Method.initialize, int.class);

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
            XpUtils.hookMethod(DPC_TAG, classLoader, new MethodHook() {
                @Override
                protected void afterHookedMethod(MethodHookParam param) {
                    mPowerState = XpUtils.getObjectField(param.thisObject, Enum.Field.mPowerState);
                    log(WAK_TAG, ((mPowerState == null ? "!!! Fail" : "Success")) + " init mPowerState");
                }
            }, Enum.Class.DisplayPowerController2, Enum.Method.initialize, int.class);

        serverThread.start();
    }

    class LocalSocketServer extends Thread {
        // 冻它命令识别码, 1359322925 是字符串"Freezeit"的10进制CRC32值
        final int baseCode = 1359322925;
        final int GET_FOREGROUND = baseCode + 1;
        final int GET_SCREEN_STATE = baseCode + 2;
        final int GET_XP_LOG = baseCode + 3;
        final int SET_CONFIG = baseCode + 20;
        final int SET_WAKEUP_LOCK = baseCode + 21; // 设置唤醒锁权限
        final int BREAK_NETWORK = baseCode + 41;
        final int UPDATE_PENDING = baseCode + 60;   // 更新待冻结应用
        final int GET_HOOK_HEALTH = baseCode + 70;
        final int GET_RUNTIME_APP_STATES = baseCode + 71;
        final int GET_SYSTEM_FREEZER_HINTS = baseCode + 72;

        // 有效命令集
        final Set<Integer> requestCodeSet = Set.of(
                GET_FOREGROUND,
                GET_SCREEN_STATE,
                GET_XP_LOG,
                SET_CONFIG,
                SET_WAKEUP_LOCK,
                BREAK_NETWORK,
                UPDATE_PENDING,
                GET_HOOK_HEALTH,
                GET_RUNTIME_APP_STATES,
                GET_SYSTEM_FREEZER_HINTS
        );

        byte[] buff = new byte[128 * 1024];// 128 KiB
        LocalServerSocket mSocketServer;

        int[] uidListTemp = new int[128]; // 临时存放UID列表，不可复用
        Exception nothingException = new Exception();

        @Override
        public void run() {
            int remainingAttempts = 4;
            while (remainingAttempts-- > 0) {
                LocalServerSocket server = null;
                try {
                    server = new LocalServerSocket("FreezeitXposedServer");
                    mSocketServer = server;
                    acceptHandle(server);
                } catch (Exception e) {
                    log(TAG, "LocalServerSocket 第" + (4 - remainingAttempts) + " 次异常: " + e);
                } finally {
                    mSocketServer = null;
                    if (server != null) {
                        try {
                            server.close();
                        } catch (IOException ignored) {
                        }
                    }
                }

                if (remainingAttempts > 0) {
                    try {
                        sleep(3000);
                    } catch (InterruptedException e) {
                        Thread.currentThread().interrupt();
                        return;
                    }
                }
            }
            log(TAG, "mSocketServer 异常次数过多，已退出");
        }

        @SuppressWarnings("InfiniteLoopStatement")
        void acceptHandle(LocalServerSocket server) throws IOException {
            while (true) {
                LocalSocket client = server.accept(); // 阻塞，监听错误必须让外层重建 socket。
                if (client == null) continue;
                try {
                    handleClient(client);
                } catch (Exception e) {
                    // 客户端帧错误不能影响监听器；accept() 本身的错误会向外层传播。
                    log(TAG, "clientHandle 异常: " + e);
                } finally {
                    try {
                        client.close();
                    } catch (Exception ignored) {
                    }
                }
            }
        }

        void handleClient(LocalSocket client) throws IOException {
            Credentials credentials = client.getPeerCredentials();
            if (credentials == null || credentials.getUid() != ROOT_UID) {
                log(TAG, "拒绝未授权 socket 客户端 uid=" +
                        (credentials == null ? "unknown" : credentials.getUid()));
                return;
            }

            InputStream is = client.getInputStream();
            final long frameDeadlineMs = SystemClock.elapsedRealtime() + SOCKET_FRAME_TIMEOUT_MS;

            if (!readFully(client, is, buff, 0, 8, frameDeadlineMs)) {
                log(TAG, "非法连接：请求头不完整");
                return;
            }

            // 前4字节是请求码，后4字节是附加数据长度
            final int requestCode = Utils.Byte2Int(buff, 0);
            if (!requestCodeSet.contains(requestCode)) {
                log(TAG, "非法请求码 " + requestCode);
                return;
            }

            final int payloadLen = Utils.Byte2Int(buff, 4);
            if (payloadLen < 0) {
                log(TAG, "非法 payloadLen " + payloadLen);
                return;
            }
            if (payloadLen > 0) {
                if (buff.length <= payloadLen) {
                    log(TAG, "数据量超过承载范围 " + payloadLen);
                    return;
                }

                if (!readFully(client, is, buff, 0, payloadLen, frameDeadlineMs)) {
                    log(TAG, "接收错误 payloadLen " + payloadLen);
                    return;
                }
            }

            var os = client.getOutputStream();
            switch (requestCode) {
                case GET_FOREGROUND:
                    handleForeground(os, buff);
                    break;
                case GET_SCREEN_STATE:
                    handleScreen(os, buff);
                    break;
                case GET_XP_LOG:
                    handleXpLog(os);
                    break;
                case SET_CONFIG:
                    handleConfig(os, buff, payloadLen);
                    break;
                case SET_WAKEUP_LOCK:
                    handleWakeupLock(os, buff, payloadLen);
                    break;
                case BREAK_NETWORK:
                    handleDestroySocket(os, buff, payloadLen);
                    break;
                case UPDATE_PENDING:
                    handlePendingApp(os, buff, payloadLen);
                    break;
                case GET_HOOK_HEALTH:
                    handleHookHealth(os);
                    break;
                case GET_RUNTIME_APP_STATES:
                    handleRuntimeAppStates(os);
                    break;
                case GET_SYSTEM_FREEZER_HINTS:
                    handleSystemFreezerHints(os);
                    break;
                default:
                    log(TAG, "请求码功能暂未实现TODO: " + requestCode);
                    break;
            }
        }


        void handleForeground(OutputStream os, byte[] replyBuff) throws IOException {
            final ArrayList<?> lruProcesses = mLruProcesses;
            final Object procLock = mProcLock;
            if (lruProcesses == null || procLock == null || !config.isCurProcStateInitialized()) {
                throw new IOException("foreground state is not initialized");
            }

            final BucketSet managedApps = config.managedApp;
            final BucketSet permissiveApps = config.permissive;
            final VectorSet nextForeground = new VectorSet(64);
            try {
                synchronized (procLock) {
                    if (mLruProcesses != lruProcesses)
                        throw new IllegalStateException("LRU process list changed while scanning");

                    for (int i = lruProcesses.size() - 1; i >= 0; i--) {
                        var processRecord = lruProcesses.get(i);
                        if (processRecord == null) continue;

                        final int uid = config.getProcessRecordUid(processRecord);// processRecord
                        if (uid < 0)
                            throw new IllegalStateException("could not read ProcessRecord UID");
                        if (!managedApps.contains(uid))
                            continue;

                        var mState = config.getProcessRecordState(processRecord);
                        if (mState == null)
                            throw new IllegalStateException("could not read ProcessRecord state");
                        int mCurProcState = config.getCurProcState(mState);
                        if (mCurProcState < 0)
                            throw new IllegalStateException("could not read current process state");

                        // 2在顶层 3绑定了顶层应用, 有前台服务:4常驻状态栏 6悬浮窗
                        // ProcessStateEnum: https://cs.android.com/android/platform/superproject/main/+/main:out/soong/.intermediates/frameworks/base/framework-minus-apex/android_common/xref35/srcjars.xref/android/app/ProcessStateEnum.java;l=10
                        if ((0 <= mCurProcState && mCurProcState <= 3) ||
                                (4 <= mCurProcState && mCurProcState <= 6 && permissiveApps.contains(uid)))
                            nextForeground.add(uid);
                    }
                }
            } catch (Exception e) {
                log(FGD_TAG, "前台服务错误: " + e);
                throw new IOException("could not scan foreground processes", e);
            }

            if (nextForeground.size() > (replyBuff.length - 4) / 4) {
                throw new IOException("foreground response exceeds bridge buffer");
            }
            if (!publishForegroundSnapshot(managedApps, nextForeground))
                throw new IOException("configuration changed while scanning foreground state");

            // 开头的4字节放置UID的个数，往后每4个字节放一个UID  [小端]
            int replyLen = (nextForeground.size() + 1) * 4;
            Utils.Int2Byte(nextForeground.size(), replyBuff, 0);
            nextForeground.toBytes(replyBuff, 4);

            os.write(replyBuff, 0, replyLen);
            os.close();
        }


        // 0未知 1息屏 2亮屏 3Doze...
        void handleScreen(OutputStream os, byte[] replyBuff) throws IOException {
            /*
            https://cs.android.com/android/platform/superproject/main/+/main:frameworks/base/core/java/android/view/Display.java;l=387
            enum DisplayStateEnum
            public static final int DISPLAY_STATE_UNKNOWN = 0;
            public static final int DISPLAY_STATE_OFF = 1;
            public static final int DISPLAY_STATE_ON = 2;
            public static final int DISPLAY_STATE_DOZE = 3; //亮屏但处于Doze的非交互状态状态
            public static final int DISPLAY_STATE_DOZE_SUSPEND = 4; // 同上，但CPU不控制显示，由协处理器或其他控制
            public static final int DISPLAY_STATE_VR = 5;
            public static final int DISPLAY_STATE_ON_SUSPEND = 6; //非Doze, 类似4
             */

            if (mPowerState == null) {
                Utils.Int2Byte(0, replyBuff, 0);
                log("屏幕状态", "mPowerState 未初始化");
            } else {
                final int mScreenState = config.getScreenState(mPowerState);
                Utils.Int2Byte(mScreenState, replyBuff, 0);
//                log("屏幕状态", String.valueOf(mScreenState));
            }

            os.write(replyBuff, 0, 4);
            os.close();
        }

        void handleXpLog(OutputStream os) throws IOException {
            os.write(XpUtils.xpLogContent.toString().getBytes());
            os.close();
        }

        void handleHookHealth(OutputStream os) throws IOException {
            boolean systemServerReady = mLruProcesses != null && mProcLock != null;
            boolean screenReady = mPowerState != null;
            boolean wakeLockReady = setUidModeMethod != null && appOpsService != null;
            boolean networkReady = mNetdService != null && UidRangeParcelClazz != null;
            boolean configReady = config != null && config.isCurProcStateInitialized();
            String json = ScopedHealthReport.systemServer(systemServerReady, configReady,
                    screenReady, wakeLockReady, networkReady, HookHealthRegistry.toJson());

            os.write(json.getBytes());
            os.close();
        }

        void handleRuntimeAppStates(OutputStream os) throws IOException {
            RuntimeSnapshot snapshot = runtimeSnapshot;
            String json = "{"
                    + "\"foreground_count\":" + snapshot.foregroundUids.size() + ","
                    + "\"pending_count\":" + snapshot.pendingUids.size() + ","
                    + "\"managed_count\":" + config.managedApp.size() + ","
                    + "\"cached_available\":" + (mLruProcesses != null && mProcLock != null)
                    + "}";
            os.write(json.getBytes());
            os.close();
        }

        void handleSystemFreezerHints(OutputStream os) throws IOException {
            boolean hookReady = mLruProcesses != null && mProcLock != null &&
                    config.isCurProcStateInitialized();
            String hint = hookReady ? "safe_control_possible" : "postpone";
            String json = "{"
                    + "\"hint\":\"" + hint + "\","
                    + "\"screen_ready\":" + (mPowerState != null) + ","
                    + "\"hook_ready\":" + hookReady
                    + "}";
            os.write(json.getBytes());
            os.close();
        }

        /**
         * 总共 2或3 行内容
         * 第一行：冻它设置数据
         * 第二行：受冻它管控的应用 只含杀死后台和冻结配置， 不含自由后台、白名单
         * 第三行：宽松前台UID列表 只含杀死后台和冻结配置， 不含自由后台、白名单 (此行可能为空)
         */
        void handleConfig(OutputStream os, byte[] buff, int payloadLen) throws IOException {
            String[] splitLine = new String(buff, 0, payloadLen).split("\n", -1);
            if (splitLine.length == 4 && splitLine[3].isEmpty())
                splitLine = Arrays.copyOf(splitLine, 3);
            int replyCode = REPLY_FAILURE;
            try {
                if (splitLine.length != 2 && splitLine.length != 3)
                    throw new IllegalArgumentException("invalid config line count: " + splitLine.length);

                StringBuilder tmp = new StringBuilder("Parse:");

                String[] elementList = splitConfigLine(splitLine[0]);
                if (elementList.length == 0 || elementList.length > config.settings.length)
                    throw new IllegalArgumentException("invalid settings count: " + elementList.length);
                int[] nextSettings = Arrays.copyOf(config.settings, config.settings.length);
                for (int i = 0; i < elementList.length; i++)
                    nextSettings[i] = Integer.parseInt(elementList[i]);
                tmp.append(" settings:").append(elementList.length);

                BucketSet nextManagedApps = new BucketSet();
                BucketSet nextPermissiveApps = new BucketSet();
                HashMap<String, Integer> nextUidIndex = new HashMap<>(512);
                HashMap<Integer, String> nextPkgIndex = new HashMap<>(512);

                elementList = splitConfigLine(splitLine[1]);
                for (String element : elementList) {
                    // Current bridge encoding is "<uid>uid<uid>". The uid prefix is
                    // variable width for secondary Android users, so never slice to five digits.
                    final int separator = element.indexOf("uid");
                    if (separator <= 0)
                        throw new IllegalArgumentException("invalid managed-app token: " + element);
                    final String uidText = element.substring(0, separator);
                    final String encodedUid = element.substring(separator + 3);
                    if (!isDecimal(uidText) || !uidText.equals(encodedUid))
                        throw new IllegalArgumentException("invalid managed-app UID: " + element);

                    final int uid = Integer.parseInt(uidText);
                    final String packName = element.substring(separator);
                    nextManagedApps.add(uid);
                    if (!nextManagedApps.contains(uid))
                        throw new IllegalArgumentException("managed UID rejected: " + uid);
                    nextUidIndex.put(packName, uid);
                    nextPkgIndex.put(uid, packName);
                }
                tmp.append(" managedApp:").append(nextManagedApps.size());
                tmp.append(" uidIndex:").append(nextUidIndex.size());
                tmp.append(" pkgIndex:").append(nextPkgIndex.size());
                nextPkgIndex.put(1000, "AndroidSystem");
                nextPkgIndex.put(-1, "Unknown");

                if (splitLine.length == 3) {
                    elementList = splitConfigLine(splitLine[2]);
                    for (String uidStr : elementList) {
                        if (!isDecimal(uidStr))
                            throw new IllegalArgumentException("invalid permissive UID: " + uidStr);
                        final int uid = Integer.parseInt(uidStr);
                        if (!nextManagedApps.contains(uid))
                            throw new IllegalArgumentException("permissive UID is unmanaged: " + uid);
                        nextPermissiveApps.add(uid);
                        if (!nextPermissiveApps.contains(uid))
                            throw new IllegalArgumentException("permissive UID rejected: " + uid);
                    }
                }
                tmp.append(" permissive:").append(nextPermissiveApps.size());

                if (!config.initField) {
                    String initResult = config.Init(classLoader);
                    if (!config.initField)
                        throw new IllegalStateException("hook field initialization failed: " + initResult);
                }

                synchronized (config) {
                    HashMap<String, Integer> currentUidIndex = config.uidIndex;
                    HashMap<Integer, String> currentPkgIndex = config.pkgIndex;
                    RuntimeSnapshot nextRuntimeSnapshot = RuntimeSnapshot.empty(nextManagedApps);
                    config.foregroundUid = nextRuntimeSnapshot.foregroundUids;
                    config.pendingUid = nextRuntimeSnapshot.pendingUids;
                    config.settings = nextSettings;
                    config.managedApp = nextManagedApps;
                    synchronized (currentUidIndex) {
                        currentUidIndex.clear();
                        currentUidIndex.putAll(nextUidIndex);
                    }
                    synchronized (currentPkgIndex) {
                        currentPkgIndex.clear();
                        currentPkgIndex.putAll(nextPkgIndex);
                    }
                    config.permissive = nextPermissiveApps;
                    runtimeSnapshot = nextRuntimeSnapshot;
                }

                log(CFG_TAG, tmp.toString());
                replyCode = REPLY_SUCCESS;
            } catch (Exception e) {
                log(CFG_TAG, "Exception: [" + Arrays.toString(splitLine) + "]: \n" + e);
            }

            Utils.Int2Byte(replyCode, buff, 0);
            os.write(buff, 0, 4);
            os.close();
        }

        private String[] splitConfigLine(String line) {
            String trimmed = line.trim();
            return trimmed.isEmpty() ? new String[0] : trimmed.split(" +");
        }

        private boolean isDecimal(String value) {
            if (value.isEmpty()) return false;
            for (int index = 0; index < value.length(); index++) {
                if (value.charAt(index) < '0' || value.charAt(index) > '9') return false;
            }
            return true;
        }


        /**
         * <a href="https://cs.android.com/android/platform/superproject/+/android-mainline-10.0.0_r9:out/soong/.intermediates/frameworks/base/framework-minus-apex/android_common/xref35/srcjars.xref/android/app/AppProtoEnums.java;l=84">...</a>
         * WAKEUP_LOCK CODE is [40] A10-A13
         * public static final String[] MODE_NAMES = new String[] {
         * "allow",        // MODE_ALLOWED
         * "ignore",       // MODE_IGNORED
         * "deny",         // MODE_ERRORED
         * "default",      // MODE_DEFAULT
         * "foreground",   // MODE_FOREGROUND
         * };
         */
        void handleWakeupLock(OutputStream os, byte[] buff, int payloadLen) throws IOException {
            final int WAKEUP_LOCK_IGNORE = 1;
            final int WAKEUP_LOCK_DEFAULT = 3;
            final int WAKEUP_LOCK_CODE = 40;

            try {
                if (setUidModeMethod == null || appOpsService == null) {
                    log(WAK_TAG, "未初始化 setUidModeMethod appOps");
                    throw nothingException;
                }

                if (payloadLen <= 8 || payloadLen % 4 != 0) {
                    log(WAK_TAG, "非法 payloadLen " + payloadLen);
                    throw nothingException;
                }

                final int uidLen = Utils.Byte2Int(buff, 0);
                if (uidLen < 0 || uidLen > uidListTemp.length ||
                        payloadLen != (uidLen + 2) * 4) {
                    log(WAK_TAG, "非法 payloadLen " + payloadLen + " uidLen " + uidLen);
                    throw nothingException;
                }

                final int mode = Utils.Byte2Int(buff, 4); // 1:ignore  3:default
                if (mode != WAKEUP_LOCK_IGNORE && mode != WAKEUP_LOCK_DEFAULT) {
                    log(WAK_TAG, "非法mode:" + mode);
                    throw nothingException;
                }

                Utils.Byte2Int(buff, 8, uidLen * 4, uidListTemp, 0);
                for (int i = 0; i < uidLen; i++) {
                    final int uid = uidListTemp[i];
                    if (config.managedApp.contains(uid))
                        setUidModeMethod.invoke(appOpsService, WAKEUP_LOCK_CODE, uid, mode);
                    else
                        log(WAK_TAG, "非法UID:" + uid);
                }

                if (XpUtils.DEBUG_WAKEUP_LOCK) {
                    var tmp = new StringBuilder(mode == WAKEUP_LOCK_IGNORE ? "禁止 WakeLock: " : "恢复 WakeLock: ");
                    for (int i = 0; i < uidLen; i++) {
                        final int uid = uidListTemp[i];
                        tmp.append(config.pkgIndex.getOrDefault(uid, String.valueOf(uid))).append(", ");
                    }
                    log(WAK_TAG, tmp.toString());
                }

                if (mode == WAKEUP_LOCK_IGNORE) { // 此操作会在息屏超时后触发
                    BucketSet managedApps = config.managedApp;
                    if (!publishForegroundSnapshot(managedApps, new VectorSet(64)))
                        log(WAK_TAG, "配置变更，跳过过期前台快照清理");
                }

                Utils.Int2Byte(REPLY_SUCCESS, buff, 0);
            } catch (Exception e) {
                Utils.Int2Byte(REPLY_FAILURE, buff, 0);
            }

            os.write(buff, 0, 4);
            os.close();
        }

        void handleDestroySocket(OutputStream os, byte[] buff, int payloadLen) throws IOException {
            try {
                if (payloadLen != 4) {
                    log(NMS_TAG, "非法 payloadLen " + payloadLen);
                    throw nothingException;
                }

                final int uid = Utils.Byte2Int(buff, 0);
                if (!config.managedApp.contains(uid)) {
                    log(NMS_TAG, "非法UID" + uid);
                    throw nothingException;
                }
                if (mNetdService == null) {
                    log(NMS_TAG, "mNetdService null");
                    throw nothingException;
                }
                if (UidRangeParcelClazz == null) {
                    log(NMS_TAG, "UidRangeParcelClazz null");
                    throw nothingException;
                }

                Object uidRanges = Array.newInstance(UidRangeParcelClazz, 1);
                Array.set(uidRanges, 0, XpUtils.newInstance(UidRangeParcelClazz, uid, uid));
                XpUtils.callMethod(mNetdService, Enum.Method.socketDestroy, uidRanges, new int[0]);
                Utils.Int2Byte(REPLY_SUCCESS, buff, 0);
            } catch (Exception e) {
                Utils.Int2Byte(REPLY_FAILURE, buff, 0);
            }

            os.write(buff, 0, 4);
            os.close();
        }

        void handlePendingApp(OutputStream os, byte[] buff, int payloadLen) throws IOException {
            try {
                if (payloadLen % 4 != 0) {
                    log(PED_TAG, "非法 payloadLen " + payloadLen);
                    throw nothingException;
                }

                final int uidLen = payloadLen / 4;
                if (uidLen > uidListTemp.length) {
                    log(PED_TAG, "待冻结 UID 数量超过承载范围 " + uidLen);
                    throw nothingException;
                }

                final BucketSet managedApps = config.managedApp;
                final VectorSet nextPending = new VectorSet(64);
                if (payloadLen > 0) {
                    Utils.Byte2Int(buff, 0, payloadLen, uidListTemp, 0);
                    for (int i = 0; i < uidLen; i++) {
                        final int uid = uidListTemp[i];
                        if (!managedApps.contains(uid))
                            throw new IllegalArgumentException("非法UID:" + uid);
                        nextPending.add(uid);
                    }
                }

                if (!publishPendingSnapshot(managedApps, nextPending))
                    throw new IllegalStateException("配置在待冻结状态同步期间发生变化");

                if (XpUtils.DEBUG_PENDING_UID) {
                    var tmp = new StringBuilder("待冻结更新: ");
                    for (int i = 0; i < uidLen; i++) {
                        final int uid = uidListTemp[i];
                        tmp.append(config.pkgIndex.getOrDefault(uid, String.valueOf(uid))).append(", ");
                    }
                    log(TAG, tmp.toString());
                }

                Utils.Int2Byte(REPLY_SUCCESS, buff, 0);
            } catch (Exception e) {
                Utils.Int2Byte(REPLY_FAILURE, buff, 0);
            }

            os.write(buff, 0, 4);
            os.close();
        }

        private boolean publishForegroundSnapshot(BucketSet expectedManagedApps,
                                                  VectorSet nextForeground) {
            synchronized (config) {
                RuntimeSnapshot current = runtimeSnapshot;
                if (config.managedApp != expectedManagedApps ||
                        current.managedApps != expectedManagedApps) {
                    return false;
                }

                long now = SystemClock.elapsedRealtime();
                config.foregroundUid = nextForeground;
                // A foreground scan starts a new daemon control cycle. The previous
                // pending set is no longer paired with this scan, so fail open until
                // UPDATE_PENDING confirms the complete state for this cycle.
                runtimeSnapshot = new RuntimeSnapshot(expectedManagedApps, nextForeground,
                        current.pendingUids, now, -1L);
                return true;
            }
        }

        private boolean publishPendingSnapshot(BucketSet expectedManagedApps,
                                               VectorSet nextPending) {
            synchronized (config) {
                RuntimeSnapshot current = runtimeSnapshot;
                if (config.managedApp != expectedManagedApps ||
                        current.managedApps != expectedManagedApps) {
                    return false;
                }

                long now = SystemClock.elapsedRealtime();
                config.pendingUid = nextPending;
                runtimeSnapshot = new RuntimeSnapshot(expectedManagedApps, current.foregroundUids,
                        nextPending, current.foregroundAtMs, now);
                return true;
            }
        }

        private boolean readFully(LocalSocket client, InputStream input, byte[] target, int offset,
                                  int length, long deadlineMs) throws IOException {
            int readCount = 0;
            while (readCount < length) {
                client.setSoTimeout(remainingFrameTimeoutMs(deadlineMs));
                int count = input.read(target, offset + readCount, length - readCount);
                if (count <= 0) return false;
                readCount += count;
            }
            return true;
        }

        private int remainingFrameTimeoutMs(long deadlineMs) throws IOException {
            long remainingMs = deadlineMs - SystemClock.elapsedRealtime();
            if (remainingMs <= 0)
                throw new IOException("socket frame deadline exceeded");
            return (int) Math.min(Integer.MAX_VALUE, Math.max(1L, remainingMs));
        }
    }
}
