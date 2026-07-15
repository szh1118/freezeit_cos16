package io.github.jark006.freezeit.fragment;

import android.annotation.SuppressLint;
import android.app.AlertDialog;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Message;
import android.text.Layout;
import android.text.method.ScrollingMovementMethod;
import android.util.Log;
import android.view.LayoutInflater;
import android.view.Menu;
import android.view.MenuInflater;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.widget.Toast;

import androidx.annotation.NonNull;
import androidx.core.view.MenuProvider;
import androidx.fragment.app.Fragment;

import java.util.Arrays;
import java.util.Timer;
import java.util.TimerTask;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.atomic.AtomicBoolean;

import io.github.jark006.freezeit.AppInfoCache;
import io.github.jark006.freezeit.ManagerCmd;
import io.github.jark006.freezeit.R;
import io.github.jark006.freezeit.Utils;
import io.github.jark006.freezeit.databinding.FragmentLogcatBinding;

public class Logcat extends Fragment {
    private static final String TAG = "Freezeit[Logcat]";

    private FragmentLogcatBinding binding;
    final int NEW_LOG_CONTENT = 1,
            UPDATE_LABEL_SUCCESS = 2,
            UPDATE_LABEL_FAIL = 3,
            LOG_REQUEST_FAILED = 4;

    Timer timer;
    private byte[] lastLogPayload;
    private int logGeneration;
    private final ExecutorService logExecutor = Executors.newSingleThreadExecutor();
    private final AtomicBoolean pollingRequestQueued = new AtomicBoolean();
    long lastTimestamp = 0;
    boolean isGetWorkLog = true;
    AlertDialog clearDialog;

    public View onCreateView(@NonNull LayoutInflater inflater,
                             ViewGroup container, Bundle savedInstanceState) {
        binding = FragmentLogcatBinding.inflate(inflater, container, false);
        binding.logView.setMovementMethod(ScrollingMovementMethod.getInstance());//流畅滑动

        requireActivity().addMenuProvider(new MenuProvider() {
            @Override
            public void onCreateMenu(@NonNull Menu menu, @NonNull MenuInflater menuInflater) {
                menu.clear();
                menuInflater.inflate(R.menu.logcat_menu, menu);
            }

            @Override
            public boolean onMenuItemSelected(@NonNull MenuItem menuItem) {
                var now = System.currentTimeMillis();
                if ((now - lastTimestamp) < 1000) {
                    Toast.makeText(requireContext(), getString(R.string.slowly_tips), Toast.LENGTH_LONG).show();
                    return true;
                }
                lastTimestamp = now;

                int id = menuItem.getItemId();
                if (id == R.id.help_log) {
                    Utils.layoutDialog(requireContext(), R.layout.help_dialog_logcat);
                } else if (id == R.id.switch_log) {
                    isGetWorkLog = !isGetWorkLog;
                    resetTimer();
                } else if (id == R.id.update_label) {
                    Toast.makeText(requireContext(), R.string.update_start, Toast.LENGTH_SHORT).show();
                    new Thread(() -> {
                        Utils.TaskResult result = Utils.freezeitTaskResult(
                                ManagerCmd.setAppLabel,
                                AppInfoCache.getAppLabelBytes());

                        handler.sendEmptyMessage((result.length() == 7 &&
                                new String(result.payload()).equals("success")) ?
                                UPDATE_LABEL_SUCCESS : UPDATE_LABEL_FAIL);
                    }).start();
                }
                return true;
            }
        }, this.getViewLifecycleOwner());

        binding.fabCheck.setOnClickListener(view -> {
            var now = System.currentTimeMillis();
            if ((now - lastTimestamp) < 1000) {
                Toast.makeText(requireContext(), getString(R.string.slowly_tips), Toast.LENGTH_LONG).show();
                return;
            }
            lastTimestamp = now;

            isGetWorkLog = true;
            runManualLogTask(ManagerCmd.printFreezerProc);
        });

        binding.fabClear.setOnClickListener(view -> {
            var now = System.currentTimeMillis();
            if ((now - lastTimestamp) < 1000) {
                Toast.makeText(requireContext(), getString(R.string.slowly_tips), Toast.LENGTH_LONG).show();
                return;
            }
            lastTimestamp = now;

            clearDialog = new AlertDialog.Builder(requireContext())
                    .setTitle(R.string.clear_log_text)
                    .setMessage(R.string.clear_log_confirm)
                    .setNegativeButton(android.R.string.cancel, null)
                    .setPositiveButton(R.string.clear_log_text, (dialog, which) -> {
                        isGetWorkLog = true;
                        runManualLogTask(ManagerCmd.clearLog);
                    })
                    .create();
            clearDialog.setOnDismissListener(dialog -> clearDialog = null);
            clearDialog.show();
        });

        return binding.getRoot();
    }

    @Override
    public void onDestroyView() {
        invalidateLogRequests();
        cancelTimer();
        handler.removeCallbacksAndMessages(null);
        if (clearDialog != null) {
            clearDialog.dismiss();
            clearDialog = null;
        }
        super.onDestroyView();
        binding = null;
    }

    @Override
    public void onPause() {
        super.onPause();
        invalidateLogRequests();
        cancelTimer();
    }

    @Override
    public void onResume() {
        super.onResume();
        resetTimer();
    }

    @Override
    public void onDestroy() {
        logExecutor.shutdownNow();
        super.onDestroy();
    }

    void cancelTimer() {
        if (timer != null) {
            timer.cancel();
            timer = null;
        }
    }

    void resetTimer() {
        cancelTimer();
        lastLogPayload = null;
        final int generation = invalidateLogRequests();
        final byte command = isGetWorkLog ? ManagerCmd.getLog : ManagerCmd.getXpLog;
        Timer pollTimer = new Timer();
        timer = pollTimer;
        pollTimer.schedule(new TimerTask() {
            @Override
            public void run() {
                enqueueLogTask(command, generation, false);
            }
        }, 0, 5000);
    }

    void runManualLogTask(byte command) {
        cancelTimer();
        int generation = invalidateLogRequests();
        enqueueLogTask(command, generation, true);
    }

    private int invalidateLogRequests() {
        return ++logGeneration;
    }

    private void enqueueLogTask(byte command, int generation, boolean restartTimer) {
        boolean isPollingRequest = !restartTimer;
        if (isPollingRequest && !pollingRequestQueued.compareAndSet(false, true))
            return;

        try {
            logExecutor.execute(() -> {
                try {
                    Utils.TaskResult result = Utils.freezeitTaskResult(command, null);
                    LogResponse response = new LogResponse(command, restartTimer, result.payload());
                    // 用 success() 区分「合法空 payload（如 ClearLog 成功后日志为空，应清屏）」
                    // 与「RPC 失败（应保留旧日志并提示）」，避免把清屏误报为失败或失败时清空旧日志。
                    handler.sendMessage(Message.obtain(handler,
                            result.success() ? NEW_LOG_CONTENT : LOG_REQUEST_FAILED,
                            generation, 0, response));
                } finally {
                    if (isPollingRequest)
                        pollingRequestQueued.set(false);
                }
            });
        } catch (RejectedExecutionException exception) {
            if (isPollingRequest)
                pollingRequestQueued.set(false);
            Log.w(TAG, "Log request ignored after executor shutdown", exception);
        }
    }

    void scrollLogToBottom() {
        if (binding == null)
            return;

        Layout layout = binding.logView.getLayout();
        if (layout != null) {
            int scrollAmount = layout.getLineTop(binding.logView.getLineCount()) - binding.logView.getHeight();
            binding.logView.scrollTo(0, Math.max(scrollAmount, 0));
        }
        binding.forBottom.requestFocus();
        binding.forBottom.clearFocus();
    }

    private final Handler handler = new Handler(Looper.getMainLooper()) {
        @SuppressLint("SetTextI18n")
        @Override
        public void handleMessage(@NonNull Message msg) {
            super.handleMessage(msg);
            if (binding == null)
                return;

            switch (msg.what) {
                case NEW_LOG_CONTENT:
                    if (msg.arg1 != logGeneration)
                        return;

                    LogResponse response = (LogResponse) msg.obj;
                    if (response.command == ManagerCmd.clearLog) {
                        lastLogPayload = new byte[0];
                        binding.logView.setText("");
                    } else if (!Arrays.equals(lastLogPayload, response.payload)) {
                        lastLogPayload = response.payload;
                        binding.logView.setText(new String(response.payload));
                        binding.logView.post(Logcat.this::scrollLogToBottom);
                    }
                    if (response.restartTimer)
                        resetTimer();
                    break;

                case LOG_REQUEST_FAILED:
                    if (msg.arg1 != logGeneration)
                        return;

                    LogResponse failedResponse = (LogResponse) msg.obj;
                    if (failedResponse.restartTimer) {
                        Toast.makeText(requireContext(), R.string.get_log_fail_tips,
                                Toast.LENGTH_SHORT).show();
                        resetTimer();
                    }
                    break;

                case UPDATE_LABEL_SUCCESS:
                    Toast.makeText(requireContext(), R.string.update_success, Toast.LENGTH_SHORT).show();
                    break;

                case UPDATE_LABEL_FAIL:
                    Toast.makeText(requireContext(), R.string.update_fail, Toast.LENGTH_SHORT).show();
                    break;
            }
        }
    };

    private static final class LogResponse {
        private final byte command;
        private final boolean restartTimer;
        private final byte[] payload;

        private LogResponse(byte command, boolean restartTimer, byte[] payload) {
            this.command = command;
            this.restartTimer = restartTimer;
            this.payload = payload;
        }

        private byte[] payload() {
            return payload;
        }
    }
}
