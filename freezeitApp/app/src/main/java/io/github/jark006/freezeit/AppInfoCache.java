package io.github.jark006.freezeit;

import static android.content.pm.ApplicationInfo.FLAG_SYSTEM;
import static android.content.pm.ApplicationInfo.FLAG_UPDATED_SYSTEM_APP;

import android.content.Context;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.graphics.drawable.Drawable;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.widget.Toast;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.TreeMap;

public class AppInfoCache {
    private static final String TAG = "Freezeit[AppInfoCache]";
    private static final int MAX_APP_LABEL_PAYLOAD_BYTES = 1024 * 1024;
    private static final int MAX_APP_LABEL_PROTOCOL_CHARS = 512;

    public static class Info {
        public Drawable icon;
        public String packName;
        public String label;
        public String forSearch;
        public boolean isSystemApp;

        public Info(Drawable icon, String packName, String label, int uid, boolean isSystemApp) {
            this.icon = icon;
            this.packName = packName;
            this.label = label;
            this.forSearch = label.toLowerCase(Locale.ENGLISH) + packName.toLowerCase(Locale.ENGLISH) + uid;
            this.isSystemApp = isSystemApp;
        }

        public boolean contains(final String keyWord) {
            return forSearch.contains(keyWord);
        }
    }

    private static final Object cacheLock = new Object();
    private static String appLabelList = "";
    private static ArrayList<Integer> uidList = new ArrayList<>(256);
    private static HashMap<Integer, Info> cacheInfo = new HashMap<>();
    private static long refreshGeneration;

    public static void refreshCache(Context context) {
        final long generation;
        synchronized (cacheLock) {
            generation = ++refreshGeneration;
        }

        PackageManager pm = context.getPackageManager();
        List<ApplicationInfo> applicationList;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            applicationList = pm.getInstalledApplications(
                    PackageManager.ApplicationInfoFlags.of(PackageManager.MATCH_UNINSTALLED_PACKAGES));
        } else {
            applicationList = pm.getInstalledApplications(PackageManager.MATCH_UNINSTALLED_PACKAGES);
        }

        TreeMap<Integer, ApplicationInfo> representativeApps = new TreeMap<>();
        for (ApplicationInfo appInfo : applicationList) {
            if (appInfo.uid < 10000)
                continue;

            ApplicationInfo current = representativeApps.get(appInfo.uid);
            if (current == null || packageNameOf(appInfo).compareTo(packageNameOf(current)) < 0)
                representativeApps.put(appInfo.uid, appInfo);
        }

        ArrayList<Integer> nextUidList = new ArrayList<>(representativeApps.size());
        HashMap<Integer, Info> nextCacheInfo = new HashMap<>(representativeApps.size());
        StringBuilder nextAppLabelList = new StringBuilder(1024 * 16);
        int labelPayloadBytes = 0;
        int truncatedLabelCount = 0;
        int omittedLabelCount = 0;
        for (Map.Entry<Integer, ApplicationInfo> entry : representativeApps.entrySet()) {
            int uid = entry.getKey();
            ApplicationInfo appInfo = entry.getValue();
            String packageName = packageNameOf(appInfo);
            CharSequence applicationLabel = pm.getApplicationLabel(appInfo);
            String label = applicationLabel == null ? packageName : applicationLabel.toString();
            boolean isSystemApp = (appInfo.flags & (FLAG_SYSTEM | FLAG_UPDATED_SYSTEM_APP)) != 0;

            nextUidList.add(uid);
            if (!packageName.equals(label)) {
                boolean labelTruncated = label.length() > MAX_APP_LABEL_PROTOCOL_CHARS;
                String sanitizedLabel = sanitizeLabelForPayload(label);
                if (labelTruncated)
                    truncatedLabelCount++;
                String labelLine = uid + " " + sanitizedLabel + '\n';
                byte[] labelLineBytes = labelLine.getBytes(StandardCharsets.UTF_8);
                if (labelLineBytes.length <= MAX_APP_LABEL_PAYLOAD_BYTES - labelPayloadBytes) {
                    nextAppLabelList.append(labelLine);
                    labelPayloadBytes += labelLineBytes.length;
                } else {
                    omittedLabelCount++;
                }
            }
            nextCacheInfo.put(uid,
                    new Info(appInfo.loadIcon(pm), packageName, label, uid, isSystemApp));
        }

        String nextAppLabelPayload = nextAppLabelList.toString();
        synchronized (cacheLock) {
            if (generation != refreshGeneration)
                return;
            uidList = nextUidList;
            cacheInfo = nextCacheInfo;
            appLabelList = nextAppLabelPayload;
        }

        int cacheSize = nextCacheInfo.size();
        Log.d(TAG, context.getString(R.string.update_cache) + cacheSize);
        if (truncatedLabelCount > 0 || omittedLabelCount > 0)
            Log.w(TAG, "Bounded app-label payload: truncated=" + truncatedLabelCount +
                    " omitted=" + omittedLabelCount);
        if (cacheSize < 2)
            new Handler(Looper.getMainLooper()).post(() ->
                    Toast.makeText(context, context.getString(R.string.appFailTips), Toast.LENGTH_LONG).show());
    }

    private static String packageNameOf(ApplicationInfo appInfo) {
        return appInfo.packageName == null ? "" : appInfo.packageName;
    }

    private static String sanitizeLabelForPayload(String label) {
        int length = Math.min(label.length(), MAX_APP_LABEL_PROTOCOL_CHARS);
        StringBuilder sanitized = new StringBuilder(length + 3);
        for (int index = 0; index < length; index++) {
            char value = label.charAt(index);
            sanitized.append(value == '\r' || value == '\n' ? ' ' : value);
        }
        if (length < label.length())
            sanitized.append("...");
        return sanitized.toString();
    }

    public static boolean contains(int uid) {
        synchronized (cacheLock) {
            return cacheInfo.containsKey(uid);
        }
    }

    public static Info get(int uid) {
        synchronized (cacheLock) {
            return cacheInfo.get(uid);
        }
    }

    public static ArrayList<Integer> getUidList() {
        synchronized (cacheLock) {
            return new ArrayList<>(uidList);
        }
    }

    public static byte[] getAppLabelBytes() {
        String labels;
        synchronized (cacheLock) {
            labels = appLabelList;
        }
        return labels.getBytes(StandardCharsets.UTF_8);
    }
}
