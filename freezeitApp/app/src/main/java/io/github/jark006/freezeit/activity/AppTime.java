package io.github.jark006.freezeit.activity;

import android.annotation.SuppressLint;
import android.content.Context;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Message;
import android.view.LayoutInflater;
import android.view.Menu;
import android.view.MenuInflater;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.widget.ImageView;
import android.widget.TextView;

import androidx.annotation.NonNull;
import androidx.appcompat.app.AppCompatActivity;
import androidx.core.view.MenuProvider;
import androidx.recyclerview.widget.DefaultItemAnimator;
import androidx.recyclerview.widget.LinearLayoutManager;
import androidx.recyclerview.widget.RecyclerView;

import java.util.Timer;
import java.util.TimerTask;

import io.github.jark006.freezeit.AppInfoCache;
import io.github.jark006.freezeit.ManagerCmd;
import io.github.jark006.freezeit.R;
import io.github.jark006.freezeit.StaticData;
import io.github.jark006.freezeit.Utils;

public class AppTime extends AppCompatActivity {
    FreezeStatusAdapter recycleAdapter = new FreezeStatusAdapter();
    Timer timer;
    final int UPDATE_DATA_SET = 1;

    @SuppressLint("MissingInflatedId")
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_app_time);

        var animator = new DefaultItemAnimator();
        animator.setSupportsChangeAnimations(false);
        RecyclerView recyclerView = findViewById(R.id.recyclerviewApp);
        recyclerView.setLayoutManager(new LinearLayoutManager(this));
        recyclerView.setAdapter(recycleAdapter);
        recyclerView.setItemAnimator(animator);
        recyclerView.setHasFixedSize(true);

        Context context = this;
        this.addMenuProvider(new MenuProvider() {
            @Override
            public void onCreateMenu(@NonNull Menu menu, @NonNull MenuInflater menuInflater) {
                menu.clear();
                menuInflater.inflate(R.menu.apptime_menu, menu);
            }

            @Override
            public boolean onMenuItemSelected(@NonNull MenuItem menuItem) {
                int id = menuItem.getItemId();
                if (id == R.id.help_task) {
                    Utils.layoutDialog(context, R.layout.help_dialog_app_time);
                }
                return true;
            }
        });
    }

    @Override
    public void onPause() {
        super.onPause();
        if (timer != null) {
            timer.cancel();
            timer = null;
        }
        handler.removeMessages(UPDATE_DATA_SET);
    }

    @Override
    public void onResume() {
        super.onResume();
        findViewById(R.id.container).setBackground(StaticData.getBackgroundDrawable(this));

        if (timer != null)
            timer.cancel();
        timer = new Timer();
        timer.schedule(new TimerTask() {
            @Override
            public void run() {
                Utils.TaskResult result = Utils.freezeitTaskResult(ManagerCmd.getFreezeStatus, null);

                // 每行状态为5个int32 [0-4]:[uid foreground state seconds processCount], 共20字节
                if (result.length() == 0 || result.length() % FreezeStatusAdapter.ROW_BYTE_LEN != 0) {
                    handler.sendMessage(Message.obtain(handler, UPDATE_DATA_SET, new int[0]));
                    return;
                }
                int[] statusRows = new int[result.length() / 4];
                Utils.Byte2Int(result.payload(), 0, result.length(), statusRows, 0);
                handler.sendMessage(Message.obtain(handler, UPDATE_DATA_SET, statusRows));
            }
        }, 0, 2000);
    }

    private final Handler handler = new Handler(Looper.getMainLooper()) {
        @SuppressLint("SetTextI18n")
        @Override
        public void handleMessage(@NonNull Message msg) {
            super.handleMessage(msg);
            if (msg.what == UPDATE_DATA_SET)
                recycleAdapter.updateDataSet((int[]) msg.obj);
        }
    };


    static class FreezeStatusAdapter extends RecyclerView.Adapter<FreezeStatusAdapter.MyViewHolder> {
        static final int ROW_INT_LEN = 5;
        static final int ROW_BYTE_LEN = ROW_INT_LEN * 4;
        static final int STATE_RUNNING_BACKGROUND = 0;
        static final int STATE_FOREGROUND = 1;
        static final int STATE_PENDING = 2;
        static final int STATE_FROZEN = 3;
        static final int STATE_TERMINATED = 4;

        int[] statusRows = new int[0];
        StringBuilder timeStr = new StringBuilder(32);

        @NonNull
        @Override
        public MyViewHolder onCreateViewHolder(@NonNull ViewGroup parent, int viewType) {
            View view = LayoutInflater.from(parent.getContext()).
                    inflate(R.layout.app_time_layout, parent, false);
            return new MyViewHolder(view);
        }

        @SuppressLint({"UseCompatLoadingForDrawables", "SetTextI18n"})
        @Override
        public void onBindViewHolder(@NonNull MyViewHolder holder, int position) {

            position *= ROW_INT_LEN;
            final int uid = statusRows[position];

            if (holder.uid != uid) {
                holder.uid = uid;
                var info = AppInfoCache.get(uid);
                if (info != null) {
                    holder.app_icon.setImageDrawable(info.icon);
                    holder.app_label.setText(info.label);
                } else {
                    holder.app_label.setText(String.valueOf(uid));
                }
            }

            final Context context = holder.itemView.getContext();
            final int foreground = statusRows[position + 1];
            final int state = statusRows[position + 2];
            final int seconds = statusRows[position + 3];
            final int processCount = statusRows[position + 4];
            holder.state.setText(getStateText(context, foreground, state, processCount));
            holder.time.setText(getTimeText(context, state, seconds));
        }

        String getStateText(@NonNull Context context, int foreground, int state, int processCount) {
            int stateRes;
            switch (state) {
                case STATE_FOREGROUND:
                    stateRes = R.string.freeze_state_foreground;
                    break;

                case STATE_PENDING:
                    stateRes = R.string.freeze_state_pending;
                    break;

                case STATE_FROZEN:
                    stateRes = R.string.freeze_state_frozen;
                    break;

                case STATE_TERMINATED:
                    stateRes = R.string.freeze_state_terminated;
                    break;

                case STATE_RUNNING_BACKGROUND:
                    stateRes = R.string.freeze_state_running;
                    break;

                default:
                    stateRes = R.string.freeze_state_unknown;
                    break;
            }

            var status = context.getString(foreground != 0 ?
                    R.string.freeze_status_foreground : R.string.freeze_status_background) +
                    " / " + context.getString(stateRes);
            if (processCount > 0)
                status += " / " + context.getString(R.string.freeze_process_count, processCount);
            return status;
        }

        String getTimeText(@NonNull Context context, int state, int seconds) {
            String duration = getTimeStr(seconds);
            switch (state) {
                case STATE_PENDING:
                    return context.getString(R.string.freeze_time_pending, duration);

                case STATE_FROZEN:
                    return context.getString(R.string.freeze_time_frozen, duration);

                case STATE_TERMINATED:
                    return context.getString(R.string.freeze_time_stopped, duration);

                case STATE_FOREGROUND:
                case STATE_RUNNING_BACKGROUND:
                    return context.getString(R.string.freeze_time_running, duration);

                default:
                    return context.getString(R.string.freeze_time_none);
            }
        }

        String getTimeStr(int time) {
            timeStr.setLength(0);

            if (time <= 0) return "0s";

            if (time >= 3600) {
                timeStr.append(time / 3600).append('h');
                time %= 3600;
            }
            if (time >= 60) {
                timeStr.append(time / 60).append('m');
                time %= 60;
            }

            timeStr.append(time).append('s');
            return timeStr.toString();
        }

        @Override
        public int getItemCount() {
            return statusRows.length / ROW_INT_LEN;
        }

        @SuppressLint("NotifyDataSetChanged")
        public void updateDataSet(@NonNull int[] newStatusRows) {
            if (statusRows.length != newStatusRows.length) {
                statusRows = newStatusRows;
                notifyDataSetChanged();
                return;
            }

            for (int i = 0; i < statusRows.length; i += ROW_INT_LEN) {
                if (statusRows[i] == newStatusRows[i] &&
                        statusRows[i + 1] == newStatusRows[i + 1] &&
                        statusRows[i + 2] == newStatusRows[i + 2] &&
                        statusRows[i + 3] == newStatusRows[i + 3] &&
                        statusRows[i + 4] == newStatusRows[i + 4])
                    continue;
                statusRows[i] = newStatusRows[i];
                statusRows[i + 1] = newStatusRows[i + 1];
                statusRows[i + 2] = newStatusRows[i + 2];
                statusRows[i + 3] = newStatusRows[i + 3];
                statusRows[i + 4] = newStatusRows[i + 4];
                notifyItemChanged(i / ROW_INT_LEN);
            }
        }

        static class MyViewHolder extends RecyclerView.ViewHolder {

            ImageView app_icon;
            TextView app_label, state, time;
            int uid = 0;

            public MyViewHolder(View view) {
                super(view);

                app_icon = view.findViewById(R.id.app_icon);
                app_label = view.findViewById(R.id.app_label);
                state = view.findViewById(R.id.state);
                time = view.findViewById(R.id.freeze_time);
            }
        }
    }
}
