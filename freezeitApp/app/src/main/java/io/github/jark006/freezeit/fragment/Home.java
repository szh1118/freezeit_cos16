package io.github.jark006.freezeit.fragment;

import android.annotation.SuppressLint;
import android.app.ActivityManager;
import android.content.ActivityNotFoundException;
import android.content.Context;
import android.content.Intent;
import android.graphics.Bitmap;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Message;
import android.util.Log;
import android.view.LayoutInflater;
import android.view.Menu;
import android.view.MenuInflater;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.view.ViewTreeObserver;
import android.widget.ImageView;
import android.widget.Toast;

import androidx.annotation.NonNull;
import androidx.core.view.MenuProvider;
import androidx.fragment.app.Fragment;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.ByteBuffer;
import java.util.Locale;
import java.util.Timer;
import java.util.TimerTask;
import java.util.concurrent.atomic.AtomicBoolean;

import io.github.jark006.freezeit.BuildConfig;
import io.github.jark006.freezeit.ManagerCmd;
import io.github.jark006.freezeit.R;
import io.github.jark006.freezeit.StaticData;
import io.github.jark006.freezeit.Utils;
import io.github.jark006.freezeit.activity.About;
import io.github.jark006.freezeit.activity.AppTime;
import io.github.jark006.freezeit.activity.Settings;
import io.github.jark006.freezeit.databinding.FragmentHomeBinding;

public class Home extends Fragment implements View.OnClickListener {
    private final static String TAG = "HomeFragment";
    private FragmentHomeBinding binding;

    Timer timer;

    final int realTimeInfoIntLen = 23;
    int[] realTimeInfo = new int[realTimeInfoIntLen]; //ARM64和X64  Native层均为小端
    boolean hasHealthStatus = false;
    String daemonHealth = "Unknown";
    String hookHealth = "Unknown";
    ImageView realTimeLayoutTarget;
    ViewTreeObserver.OnGlobalLayoutListener realTimeLayoutListener;
    private final AtomicBoolean onlineInfoInFlight = new AtomicBoolean(false);
    private volatile boolean viewResumed;
    private volatile int realTimeGeneration;
    private int statusGeneration;

    private static final class ModuleInfo {
        final int moduleVersionCode;
        final int clusterType;
        final int extMemory;
        final String moduleVersion;
        final String moduleEnv;
        final String workMode;
        final String androidVer;
        final String kernelVer;
        final boolean hasHealthStatus;
        final String daemonHealth;
        final String hookHealth;

        ModuleInfo(int moduleVersionCode, int clusterType, int extMemory,
                   String moduleVersion, String moduleEnv, String workMode,
                   String androidVer, String kernelVer, boolean hasHealthStatus,
                   String daemonHealth, String hookHealth) {
            this.moduleVersionCode = moduleVersionCode;
            this.clusterType = clusterType;
            this.extMemory = extMemory;
            this.moduleVersion = moduleVersion;
            this.moduleEnv = moduleEnv;
            this.workMode = workMode;
            this.androidVer = androidVer;
            this.kernelVer = kernelVer;
            this.hasHealthStatus = hasHealthStatus;
            this.daemonHealth = daemonHealth;
            this.hookHealth = hookHealth;
        }
    }

    public View onCreateView(@NonNull LayoutInflater inflater,
                             ViewGroup container, Bundle savedInstanceState) {

        binding = FragmentHomeBinding.inflate(inflater, container, false);
        binding.cpuImg.setScaleType(ImageView.ScaleType.FIT_XY);

        binding.downloadButton.setOnClickListener(this);
        binding.realtimeLayout.setOnClickListener(this);

        requireActivity().addMenuProvider(new MenuProvider() {
            @Override
            public void onCreateMenu(@NonNull Menu menu, @NonNull MenuInflater menuInflater) {
                menu.clear();
                menuInflater.inflate(R.menu.home_menu, menu);
            }

            @Override
            public boolean onMenuItemSelected(@NonNull MenuItem menuItem) {
                int id = menuItem.getItemId();
                if (id == R.id.settings) {
                    if (StaticData.hasGetPropInfo)
                        startActivity(new Intent(requireContext(), Settings.class));
                    else
                        Toast.makeText(requireContext(), getString(R.string.freezeit_offline), Toast.LENGTH_LONG).show();
                } else if (id == R.id.about) {
                    startActivity(new Intent(requireContext(), About.class));
                }
                return true;
            }
        }, this.getViewLifecycleOwner());

        binding.swipeRefreshLayout.setOnRefreshListener(() -> {
            StaticData.hasGetPropInfo = false;
            StaticData.hasOnlineInfo = false;
            StaticData.onlineChangelog = "";
            StaticData.localChangelog = "";
            refreshStatus();
        });

        return binding.getRoot();
    }

    @Override
    public void onPause() {
        viewResumed = false;
        statusGeneration++;
        super.onPause();
        cancelRealTimeTimer();
    }

    @Override
    public void onResume() {
        super.onResume();
        viewResumed = true;
        refreshStatus();
    }

    @Override
    public void onDestroyView() {
        viewResumed = false;
        statusGeneration++;
        super.onDestroyView();
        cancelRealTimeTimer();
        clearRealTimeLayoutListener();
        handler.removeCallbacksAndMessages(null);
        binding = null;
    }

    final int HAS_MODULE_INFO = 1,
            NO_MODULE_INFO = 2,
            HANDLE_ONLINE_INFO = 4,
            HANDLE_REALTIME_INFO = 5,
            UPDATE_ONLINE_CHANGELOG = 7,
            UPDATE_LOCAL_CHANGELOG = 8;

    void refreshStatus() {
        final int requestGeneration = ++statusGeneration;
        StaticData.hasGetPropInfo = false;
        new Thread(() -> {
            Utils.TaskResult result = Utils.freezeitTaskResult(ManagerCmd.getPropInfo, null);

            // info [0]:moduleID [1]:moduleName [2]:moduleVersion [3]:moduleVersionCode [4]:moduleAuthor
            //      [5]:clusterNum: CPU丛集数
            //      [6]:moduleEnv:  Magisk or KernelSU
            //      [7]:workMode:   冻结模式 V1 / V2
            //      [8]:androidVer: 安卓版本
            //      [9]:kernelVer:  内核版本
            //      [10]:extMemory: 内存扩展 MiB
            //      [11]:daemonHealth: Rust daemon active/degraded/inactive
            //      [12]:hookHealth: LSPosed bridge active/degraded/inactive/unknown
            ModuleInfo moduleInfo = parseModuleInfo(result);
            Message message = handler.obtainMessage(
                    moduleInfo == null ? NO_MODULE_INFO : HAS_MODULE_INFO,
                    moduleInfo);
            message.arg1 = requestGeneration;
            message.sendToTarget();
        }).start();
    }

    private static ModuleInfo parseModuleInfo(Utils.TaskResult result) {
        if (result.length() == 0)
            return null;

        String[] info = new String(result.payload()).split("\n");
        if (info.length < 5)
            return null;

        int moduleVersionCode = 0;
        int clusterType = 0;
        int extMemory = 0;
        try {
            moduleVersionCode = Integer.parseInt(info[3]);
            clusterType = info.length > 5 ? Integer.parseInt(info[5]) : 0;
            extMemory = info.length > 10 ? Integer.parseInt(info[10]) : 0;
        } catch (Exception ignored) {
        }
        return new ModuleInfo(
                moduleVersionCode,
                clusterType,
                extMemory,
                info[2],
                info.length > 6 ? info[6] : "Unknown",
                info.length > 7 ? info[7] : "Unknown",
                info.length > 8 ? info[8] : "Unknown",
                info.length > 9 ? info[9] : "Unknown",
                info.length > 11,
                info.length > 11 ? info[11] : "Unknown",
                info.length > 12 ? info[12] : "Unknown"
        );
    }

    private void applyModuleInfo(ModuleInfo moduleInfo) {
        StaticData.moduleVersionCode = moduleInfo.moduleVersionCode;
        StaticData.clusterType = moduleInfo.clusterType;
        StaticData.extMemory = moduleInfo.extMemory;
        StaticData.moduleVersion = moduleInfo.moduleVersion;
        StaticData.moduleEnv = moduleInfo.moduleEnv;
        StaticData.workMode = moduleInfo.workMode;
        StaticData.androidVer = moduleInfo.androidVer;
        StaticData.kernelVer = moduleInfo.kernelVer;
        hasHealthStatus = moduleInfo.hasHealthStatus;
        daemonHealth = moduleInfo.daemonHealth;
        hookHealth = moduleInfo.hookHealth;
        StaticData.hasGetPropInfo = true;
    }

    void getOnlineInfoTask() {
        if (StaticData.hasOnlineInfo) {
            handler.sendEmptyMessage(HANDLE_ONLINE_INFO);
            return;
        }
        if (!onlineInfoInFlight.compareAndSet(false, true))
            return;

        Context context = getContext();
        if (context == null) {
            onlineInfoInFlight.set(false);
            return;
        }
        final String updateJsonLink = context.getString(R.string.update_json_link);
        new Thread(() -> {
            try {
                byte[] response = getNetworkDataWithTimeout(updateJsonLink);
                if (response == null || response.length == 0)
                    return;
                try {
                    JSONObject json = new JSONObject(new String(response));
                    StaticData.onlineVersion = json.getString("version");
                    StaticData.onlineVersionCode = json.getInt("versionCode");
                    StaticData.zipUrl = json.getString("zipUrl");
                    StaticData.changelogUrl = json.getString("changelog");
                    StaticData.hasOnlineInfo = true;
                    handler.sendEmptyMessage(HANDLE_ONLINE_INFO);
                } catch (JSONException e) {
                    Log.e(TAG, e.toString());
                }
            } finally {
                onlineInfoInFlight.set(false);
            }
        }).start();
    }

    private static byte[] getNetworkDataWithTimeout(String link) {
        HttpURLConnection connection = null;
        try {
            connection = (HttpURLConnection) new URL(link).openConnection();
            connection.setConnectTimeout(5 * 1000);
            connection.setReadTimeout(10 * 1000);
            connection.setRequestMethod("GET");
            connection.connect();
            if (connection.getResponseCode() != HttpURLConnection.HTTP_OK)
                return null;

            try (InputStream input = connection.getInputStream();
                 ByteArrayOutputStream output = new ByteArrayOutputStream()) {
                byte[] buffer = new byte[4096];
                int length;
                while ((length = input.read(buffer)) != -1)
                    output.write(buffer, 0, length);
                return output.toByteArray();
            }
        } catch (IOException | ClassCastException e) {
            Log.e(TAG, "Network request failed", e);
            return null;
        } finally {
            if (connection != null)
                connection.disconnect();
        }
    }

    void initRealTimeInfoTimer() {
        if (!viewResumed || timer != null || StaticData.imgHeight == 0 || StaticData.imgWidth == 0)
            return;

        final byte[] realTimeRequest = new byte[12];
        Utils.Int2Byte(StaticData.imgHeight, realTimeRequest, 0);
        Utils.Int2Byte(StaticData.imgWidth, realTimeRequest, 4);

        final int requestGeneration = ++realTimeGeneration;
        timer = new Timer();
        timer.schedule(new TimerTask() {
            @Override
            public void run() {
                if (!viewResumed || requestGeneration != realTimeGeneration || StaticData.am == null)
                    return;

                ActivityManager.MemoryInfo memoryInfo = new ActivityManager.MemoryInfo();
                StaticData.am.getMemoryInfo(memoryInfo); // 底层 /proc/meminfo 的 MemAvailable 不可靠
                Utils.Int2Byte((int) (memoryInfo.availMem >> 20), realTimeRequest, 8); //Unit: MiB

                if (!viewResumed || requestGeneration != realTimeGeneration)
                    return;
                Utils.TaskResult result = Utils.freezeitTaskResult(
                        ManagerCmd.getRealTimeInfo,
                        realTimeRequest);
                if (result.length() > 0 && viewResumed && requestGeneration == realTimeGeneration) {
                    Message message = handler.obtainMessage(HANDLE_REALTIME_INFO, result.payload());
                    message.arg1 = requestGeneration;
                    message.sendToTarget();
                }
            }
        }, 0, 3000);
    }

    private final Handler handler = new Handler(Looper.getMainLooper()) {
        @SuppressLint({"SetTextI18n", "DefaultLocale"})
        @Override
        public void handleMessage(@NonNull Message msg) {
            super.handleMessage(msg);
            if (binding == null || !viewResumed)
                return;

            // 状态机
            switch (msg.what) {
                case NO_MODULE_INFO: {
                    if (msg.arg1 != statusGeneration)
                        return;
                    binding.swipeRefreshLayout.setRefreshing(false);
                    binding.stateLayout.setBackgroundResource(R.color.warn_red);
                    binding.statusText.setText(R.string.freezeit_error_tips);
                    binding.realtimeLayout.setVisibility(View.GONE);
                    binding.freezeitLogo.setVisibility(View.GONE);
                    binding.versionCard.setVisibility(View.GONE);
                    getOnlineInfoTask();
                }
                break;

                case HANDLE_ONLINE_INFO: {
                    if (StaticData.moduleVersionCode == StaticData.onlineVersionCode) {
                        binding.changelogLayout.setVisibility(View.GONE);
                        return;
                    } else if (StaticData.moduleVersionCode < StaticData.onlineVersionCode) {
                        binding.downloadButton.setVisibility(View.VISIBLE);
                        binding.changelogLayout.setVisibility(View.VISIBLE);
                        String tmp = requireContext().getString(StaticData.moduleVersionCode == 0 ?
                                R.string.online_version : R.string.new_version) + " " + StaticData.onlineVersion +
                                " (" + StaticData.onlineVersionCode + ")";
                        binding.versionText.setText(tmp);

                        if (StaticData.onlineChangelog.length() > 0)
                            binding.changelogText.setText(StaticData.onlineChangelog);
                        else if (StaticData.changelogUrl.length() > 0)
                            new Thread(() -> {
                                var response = getNetworkDataWithTimeout(StaticData.changelogUrl);
                                if (response == null || response.length == 0)
                                    return;

                                var split = new String(response).split("###");
                                if (split.length > 1 && split[1].length() > 2) {
                                    StaticData.onlineChangelog = split[1].trim();
                                    this.sendEmptyMessage(UPDATE_ONLINE_CHANGELOG);
                                }
                            }).start();
                    } else {
                        binding.downloadButton.setVisibility(View.GONE);
                        binding.changelogLayout.setVisibility(View.VISIBLE);

                        var sb = new StringBuilder();
                        sb.append(getString(R.string.beta_version)).append(": ").append(StaticData.moduleVersion)
                                .append(" (").append(StaticData.moduleVersionCode).append(")\n");
                        sb.append(getString(R.string.online_version)).append(": ").append(StaticData.onlineVersion)
                                .append(" (").append(StaticData.onlineVersionCode).append(")\n");
                        binding.versionText.setText(sb);

                        if (StaticData.localChangelog.length() > 0)
                            binding.changelogText.setText(StaticData.localChangelog);
                        else new Thread(() -> {
                            Utils.TaskResult result = Utils.freezeitTaskResult(ManagerCmd.getChangelog, null);
                            if (result.length() == 0)
                                return;

                            var split = new String(result.payload()).split("###");
                            if (split.length > 1 && split[1].length() > 2) {
                                StaticData.localChangelog = split[1].trim();
                                this.sendEmptyMessage(UPDATE_LOCAL_CHANGELOG);
                            }
                        }).start();
                    }
                }
                break;

                case UPDATE_ONLINE_CHANGELOG:
                    binding.changelogText.setText(StaticData.onlineChangelog);
                    break;

                case UPDATE_LOCAL_CHANGELOG:
                    binding.changelogText.setText(StaticData.localChangelog);
                    break;

                case HAS_MODULE_INFO: {
                    if (msg.arg1 != statusGeneration)
                        return;
                    applyModuleInfo((ModuleInfo) msg.obj);
                    binding.swipeRefreshLayout.setRefreshing(false);
                    binding.freezeitLogo.setVisibility(View.VISIBLE);
                    binding.realtimeLayout.setVisibility(View.VISIBLE);
                    binding.versionCard.setVisibility(View.VISIBLE);

                    boolean xposedState = isXposedActive();
                    if (hasHealthStatus) {
                        boolean daemonActive = "active".equalsIgnoreCase(daemonHealth);
                        boolean hookActive = xposedState || "active".equalsIgnoreCase(hookHealth);
                        boolean ready = daemonActive && hookActive;
                        binding.stateLayout.setBackgroundResource(ready ? R.color.normal_green : R.color.warn_orange);
                        binding.statusText.setText(getString(
                                R.string.health_status_format,
                                localizedHealthStatus(daemonHealth),
                                localizedHealthStatus(hookHealth)
                        ));
                    } else {
                        binding.stateLayout.setBackgroundResource(xposedState ? R.color.normal_green : R.color.warn_orange);
                        binding.statusText.setText(xposedState ? StaticData.workMode : "Xposed " + getString(R.string.xposed_warn));
                    }

                    binding.moduleEnv.setText(StaticData.moduleEnv);
                    binding.moduleVer.setText(StaticData.moduleVersion + " (" + StaticData.moduleVersionCode + ")");
                    binding.managerVer.setText(BuildConfig.VERSION_NAME + " (" + BuildConfig.VERSION_CODE + ")");
                    binding.androidVer.setText(StaticData.androidVer);
                    binding.kernelVer.setText(StaticData.kernelVer);

                    getOnlineInfoTask();

                    if (StaticData.imgWidth != 0 && StaticData.imgHeight != 0) {
                        initRealTimeInfoTimer();
                        return;
                    }
                    if (binding.cpuImg.getWidth() > 0 && binding.cpuImg.getHeight() > 0) {
                        initializeRealTimeDimensions();
                        return;
                    }

                    clearRealTimeLayoutListener();
                    realTimeLayoutTarget = binding.cpuImg;
                    realTimeLayoutListener = new ViewTreeObserver.OnGlobalLayoutListener() {
                        @Override
                        public void onGlobalLayout() {
                            ImageView target = realTimeLayoutTarget;
                            if (target != null) {
                                ViewTreeObserver observer = target.getViewTreeObserver();
                                if (observer.isAlive())
                                    observer.removeOnGlobalLayoutListener(this);
                            }
                            realTimeLayoutTarget = null;
                            realTimeLayoutListener = null;
                            initializeRealTimeDimensions();
                        }
                    };
                    realTimeLayoutTarget.getViewTreeObserver()
                            .addOnGlobalLayoutListener(realTimeLayoutListener);
                }
                break;

                case HANDLE_REALTIME_INFO:
                    if (msg.arg1 == realTimeGeneration)
                        realTimeHandleFunc((byte[]) msg.obj);
                    break;
            }
        }
    };

    private void cancelRealTimeTimer() {
        realTimeGeneration++;
        if (timer != null) {
            timer.cancel();
            timer = null;
        }
    }

    private void clearRealTimeLayoutListener() {
        if (realTimeLayoutTarget != null && realTimeLayoutListener != null) {
            ViewTreeObserver observer = realTimeLayoutTarget.getViewTreeObserver();
            if (observer.isAlive())
                observer.removeOnGlobalLayoutListener(realTimeLayoutListener);
        }
        realTimeLayoutTarget = null;
        realTimeLayoutListener = null;
    }

    private void initializeRealTimeDimensions() {
        if (binding == null || binding.cpuImg.getWidth() <= 0 || binding.cpuImg.getHeight() <= 0)
            return;

        int width = binding.cpuImg.getWidth() / StaticData.imgScale;
        int height = binding.cpuImg.getHeight() / StaticData.imgScale;
        while (width * height > 1024 * 1024) { // RGBA 最多预留 4MiB 用于绘图
            StaticData.imgScale++;
            width = binding.cpuImg.getWidth() / StaticData.imgScale;
            height = binding.cpuImg.getHeight() / StaticData.imgScale;
        }
        StaticData.imgWidth = width;
        StaticData.imgHeight = height;
        initRealTimeInfoTimer();
    }

    private String localizedHealthStatus(String health) {
        if (health == null)
            return getString(R.string.health_status_unknown);

        switch (health.toLowerCase(Locale.ROOT)) {
            case "active":
                return getString(R.string.health_status_active);
            case "degraded":
                return getString(R.string.health_status_degraded);
            case "inactive":
                return getString(R.string.health_status_inactive);
            default:
                return getString(R.string.health_status_unknown);
        }
    }


    @SuppressLint({"DefaultLocale", "SetTextI18n"})
    void realTimeHandleFunc(@NonNull byte[] response) {

        // response[0 ~ imgBuffBytes-1]CPU曲线图像数据, [imgBuffBytes ~ end]是其他实时数据
        int imgBuffBytes = StaticData.imgWidth * StaticData.imgHeight * 4; // ARGB 每像素4字节
        if (response.length <= imgBuffBytes) {
            String errorTips = "imgWidth" + StaticData.imgWidth +
                    " imgHeight" + StaticData.imgHeight +
                    " response.length" + response.length +
                    " imgBuffBytes" + imgBuffBytes;
            binding.cpu.setText(errorTips);
            return;
        }

        if (StaticData.bitmap == null || StaticData.bitmap.getHeight() != StaticData.imgHeight ||
                StaticData.bitmap.getWidth() != StaticData.imgWidth)
            StaticData.bitmap = Bitmap.createBitmap(StaticData.imgWidth, StaticData.imgHeight, Bitmap.Config.ARGB_8888);

        StaticData.bitmap.copyPixelsFromBuffer(ByteBuffer.wrap(response, 0, imgBuffBytes));
        binding.cpuImg.setImageBitmap(StaticData.bitmap);

        int realTimeInfoByteLen = response.length - imgBuffBytes;
        if (realTimeInfoByteLen != 4 * realTimeInfoIntLen) {
            String errorTips = "Required bytes: " + (4 * realTimeInfoIntLen) + " Received bytes:" + realTimeInfoByteLen;
            binding.cpu.setText(errorTips);
            return;
        }

        // [0]全部物理内存 [1]可用内存 [2]全部虚拟内存 [3]可用虚拟内存  Unit: MiB
        // [4-11]八个核心频率(MHz) [12-19]八个核心使用率(%)
        // [20]CPU总使用率(%) [21]CPU温度(m℃) [22]电池功率(mW)
        Utils.Byte2Int(response, imgBuffBytes, realTimeInfoIntLen * 4, realTimeInfo, 0);

        final double GiB = 1024.0;
        int MemTotal = realTimeInfo[0], MemAvailable = realTimeInfo[1];
        int SwapTotal = realTimeInfo[2], SwapFree = realTimeInfo[3];

        @SuppressLint("DefaultLocale")
        String tmp = MemTotal <= 0 ? "" : String.format(getString(R.string.physical_ram_text),
                MemTotal / GiB, 100.0 * (MemTotal - MemAvailable) / MemTotal,
                MemAvailable > GiB ? MemAvailable / GiB : MemAvailable,
                MemAvailable > GiB ? "GiB" : "MiB");
        binding.memInfo.setText(tmp);

        tmp = SwapTotal <= 0 ? "" : String.format(getString(R.string.virtual_ram_text),
                SwapTotal / GiB, 100.0 * (SwapTotal - SwapFree) / SwapTotal,
                SwapFree > GiB ? SwapFree / GiB : SwapFree,
                SwapFree > GiB ? "GiB" : "MiB");
        if (StaticData.extMemory > 0)
            tmp += "\n" + String.format(getString(R.string.ext_memory), StaticData.extMemory / 1024.0);
        binding.zramInfo.setText(tmp);

        final int percent = realTimeInfo[20];
        final double temperature = realTimeInfo[21] / 1e3; // m℃ -> ℃
        final int mW = realTimeInfo[22]; // mW 毫瓦
        binding.cpu.setText(String.format(getString(R.string.cpu_format), percent, temperature));
        binding.battery.setText(String.format("%.2f W\uD83D\uDD0B", mW / 1e3));

        binding.cpu0.setText("cpu0\n" + realTimeInfo[4] + "MHz\n" + realTimeInfo[12] + "%");
        binding.cpu1.setText("cpu1\n" + realTimeInfo[5] + "MHz\n" + realTimeInfo[13] + "%");
        binding.cpu2.setText("cpu2\n" + realTimeInfo[6] + "MHz\n" + realTimeInfo[14] + "%");
        binding.cpu3.setText("cpu3\n" + realTimeInfo[7] + "MHz\n" + realTimeInfo[15] + "%");
        binding.cpu4.setText("cpu4\n" + realTimeInfo[8] + "MHz\n" + realTimeInfo[16] + "%");
        binding.cpu5.setText("cpu5\n" + realTimeInfo[9] + "MHz\n" + realTimeInfo[17] + "%");
        binding.cpu6.setText("cpu6\n" + realTimeInfo[10] + "MHz\n" + realTimeInfo[18] + "%");
        binding.cpu7.setText("cpu7\n" + realTimeInfo[11] + "MHz\n" + realTimeInfo[19] + "%");
    }

    @Override
    public void onClick(View v) {
        int id = v.getId();
        if (id == R.id.realtimeLayout) {
            startActivity(new Intent(requireContext(), AppTime.class));
        } else if (id == R.id.download_button) {
            openDownload();
        }
    }

    private void openDownload() {
        Context context = getContext();
        if (context == null)
            return;

        try {
            Uri uri = Uri.parse(StaticData.zipUrl);
            String scheme = uri.getScheme();
            if (uri.getHost() == null || (!"https".equalsIgnoreCase(scheme) &&
                    !"http".equalsIgnoreCase(scheme))) {
                Toast.makeText(context, R.string.update_fail, Toast.LENGTH_LONG).show();
                return;
            }
            Intent intent = new Intent(Intent.ACTION_VIEW, uri);
            if (intent.resolveActivity(context.getPackageManager()) == null) {
                Toast.makeText(context, R.string.update_fail, Toast.LENGTH_LONG).show();
                return;
            }
            startActivity(intent);
        } catch (ActivityNotFoundException | SecurityException | IllegalArgumentException e) {
            Toast.makeText(context, R.string.update_fail, Toast.LENGTH_LONG).show();
        }
    }

    public boolean isXposedActive() {
        Log.e(TAG, "isXposedActive: Hook Fail");
        return false;
    }

}
