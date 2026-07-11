package io.github.jark006.freezeit.fragment;

import android.annotation.SuppressLint;
import android.app.AlertDialog;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Message;
import android.text.Layout;
import android.text.method.ScrollingMovementMethod;
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

import java.util.Timer;
import java.util.TimerTask;

import io.github.jark006.freezeit.AppInfoCache;
import io.github.jark006.freezeit.ManagerCmd;
import io.github.jark006.freezeit.R;
import io.github.jark006.freezeit.Utils;
import io.github.jark006.freezeit.databinding.FragmentLogcatBinding;

public class Logcat extends Fragment {
    private FragmentLogcatBinding binding;
    final int NEW_LOG_CONTENT = 1,
            UPDATE_LABEL_SUCCESS = 2,
            UPDATE_LABEL_FAIL = 3;

    Timer timer;
    int lastLogLen = 0;
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
                    cancelTimer();
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
            new Thread(() -> logTask(ManagerCmd.printFreezerProc)).start();
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
                        new Thread(() -> logTask(ManagerCmd.clearLog)).start();
                    })
                    .create();
            clearDialog.setOnDismissListener(dialog -> clearDialog = null);
            clearDialog.show();
        });

        return binding.getRoot();
    }

    @Override
    public void onDestroyView() {
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
        cancelTimer();
    }

    @Override
    public void onResume() {
        super.onResume();
        resetTimer();
    }

    void cancelTimer() {
        if (timer != null) {
            timer.cancel();
            timer = null;
        }
    }

    void resetTimer() {
        cancelTimer();
        lastLogLen = 0;
        timer = new Timer();
        timer.schedule(new TimerTask() {
            @Override
            public void run() {
                logTask(isGetWorkLog ? ManagerCmd.getLog : ManagerCmd.getXpLog);
            }
        }, 0, 5000);
    }

    void logTask(byte cmd) {
        Utils.TaskResult result = Utils.freezeitTaskResult(cmd, null);
        if (result.length() == 0 || result.length() == lastLogLen)
            return;
        lastLogLen = result.length();
        handler.sendMessage(Message.obtain(handler, NEW_LOG_CONTENT, result.payload()));
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
                    binding.logView.setText(new String((byte[]) msg.obj));
                    binding.logView.post(Logcat.this::scrollLogToBottom);
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
}
