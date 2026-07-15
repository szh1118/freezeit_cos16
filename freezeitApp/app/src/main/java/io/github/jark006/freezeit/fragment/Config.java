package io.github.jark006.freezeit.fragment;

import static io.github.jark006.freezeit.Utils.CFG_FREEZER;
import static io.github.jark006.freezeit.Utils.CFG_FREEZER_BR;
import static io.github.jark006.freezeit.Utils.CFG_SIGSTOP;
import static io.github.jark006.freezeit.Utils.CFG_SIGSTOP_BR;
import static io.github.jark006.freezeit.Utils.CFG_TERMINATE;
import static io.github.jark006.freezeit.Utils.CFG_WHITEFORCE;
import static io.github.jark006.freezeit.Utils.CFG_WHITELIST;

import android.annotation.SuppressLint;
import android.content.Context;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Message;
import android.os.SystemClock;
import android.util.Log;
import android.util.Pair;
import android.view.LayoutInflater;
import android.view.Menu;
import android.view.MenuInflater;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.widget.AdapterView;
import android.widget.ImageView;
import android.widget.Spinner;
import android.widget.TextView;
import android.widget.Toast;

import androidx.annotation.NonNull;
import androidx.appcompat.widget.SearchView;
import androidx.core.view.MenuProvider;
import androidx.fragment.app.Fragment;
import androidx.recyclerview.widget.DefaultItemAnimator;
import androidx.recyclerview.widget.LinearLayoutManager;
import androidx.recyclerview.widget.RecyclerView;

import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

import io.github.jark006.freezeit.AppInfoCache;
import io.github.jark006.freezeit.ManagerCmd;
import io.github.jark006.freezeit.R;
import io.github.jark006.freezeit.Utils;
import io.github.jark006.freezeit.databinding.FragmentConfigBinding;

public class Config extends Fragment {
    private final static String TAG = "ConfigFragment";
    private static final ExecutorService CONFIG_SAVE_EXECUTOR = Executors.newSingleThreadExecutor();
    final int GET_APP_CFG = 1,
            SET_CFG_SUCCESS = 2,
            SET_CFG_FAIL = 3;

    private FragmentConfigBinding binding;
    private final AppCfgAdapter recycleAdapter = new AppCfgAdapter();
    private long lastTimestamp = 0;
    private long configLoadGeneration = 0;

    public View onCreateView(@NonNull LayoutInflater inflater,
                             ViewGroup container, Bundle savedInstanceState) {

        binding = FragmentConfigBinding.inflate(inflater, container, false);

        binding.recyclerviewApp.setLayoutManager(new LinearLayoutManager(requireContext()));
        var animator = new DefaultItemAnimator();
        animator.setSupportsChangeAnimations(false);
        binding.recyclerviewApp.setItemAnimator(animator);
        binding.recyclerviewApp.setAdapter(recycleAdapter);
        binding.recyclerviewApp.setHasFixedSize(true);
        binding.fabSave.setEnabled(false);

        binding.swipeRefreshLayout.setOnRefreshListener(this::startConfigLoad);

        recycleAdapter.setContext(requireContext());

        requireActivity().addMenuProvider(new MenuProvider() {
            @Override
            public void onCreateMenu(@NonNull Menu menu, @NonNull MenuInflater menuInflater) {
                menu.clear();
                menuInflater.inflate(R.menu.config_menu, menu);
                SearchView searchView = (SearchView) menu.findItem(R.id.search_view).getActionView();
                if (searchView == null) {
                    Log.e(TAG, "onCreateMenu: searchView == null");
                    return;
                }
                searchView.setOnQueryTextListener(new SearchView.OnQueryTextListener() {
                    @Override
                    public boolean onQueryTextSubmit(String query) {//按下搜索触发
                        return true;
                    }

                    @Override
                    public boolean onQueryTextChange(String newText) {
                        recycleAdapter.filter(newText != null ? newText.toLowerCase(Locale.ENGLISH) : "");
                        return true;
                    }
                });

            }

            @Override
            public boolean onMenuItemSelected(@NonNull MenuItem menuItem) {
                int id = menuItem.getItemId();
                if (id == R.id.help_config) {
                    Utils.layoutDialog(requireContext(), R.layout.help_dialog_config);
                }
                return true;
            }
        }, this.getViewLifecycleOwner());

        binding.fabSave.setOnClickListener(view -> {
            var now = SystemClock.elapsedRealtime();
            if ((now - lastTimestamp) < 1000) {
                Toast.makeText(requireContext(), getString(R.string.slowly_tips), Toast.LENGTH_LONG).show();
                return;
            }
            lastTimestamp = now;

            AppCfgAdapter.SaveRequest saveRequest = recycleAdapter.createSaveRequest();
            if (saveRequest == null) return;

            CONFIG_SAVE_EXECUTOR.execute(() -> {
                Utils.TaskResult result = Utils.freezeitTaskResult(
                        ManagerCmd.setAppCfg, saveRequest.payload);
                handler.obtainMessage((result.length() == 7 &&
                        new String(result.payload()).equals("success")) ?
                        SET_CFG_SUCCESS : SET_CFG_FAIL, saveRequest).sendToTarget();
            });
        });

        binding.fabSwitchSys.setOnClickListener(view -> {
            var now = SystemClock.elapsedRealtime();
            if ((now - lastTimestamp) < 500) {
                Toast.makeText(requireContext(), getString(R.string.slowly_tips), Toast.LENGTH_LONG).show();
                return;
            }
            lastTimestamp = now;

            recycleAdapter.switchAppType();
        });

        return binding.getRoot();
    }

    @Override
    public void onResume() {
        super.onResume();

        startConfigLoad();
    }

    private void startConfigLoad() {
        final long generation = beginConfigLoad();
        // 在主线程取 context 再传入后台线程；若在线程体内调 requireContext()，
        // Fragment detach（onDetach 置空 mContext）后会抛 IllegalStateException 崩溃。
        final Context context = requireContext().getApplicationContext();
        new Thread(() -> {
            AppInfoCache.refreshCache(context);
            getAppCfgTask(generation);
        }).start();
    }

    private long beginConfigLoad() {
        long generation = ++configLoadGeneration;
        recycleAdapter.beginLoading();
        if (binding != null) {
            binding.fabSave.setEnabled(false);
            binding.swipeRefreshLayout.setRefreshing(true);
        }
        return generation;
    }

    private void getAppCfgTask(long generation) {
        Utils.TaskResult result = Utils.freezeitTaskResult(ManagerCmd.getAppCfg, null);
        int recvLen = result.length();
        if (recvLen == 0 || recvLen % 12 != 0) {
            postConfigLoadResult(generation, null);
            return;
        }
        byte[] response = result.payload();

        HashMap<Integer, Pair<Integer, Integer>> loadedAppCfg = new HashMap<>();
        ArrayList<Integer> loadedPersistedUidOrder = new ArrayList<>();
        // 每个配置含：3个[int32]数据，12字节 小端
        for (int i = 0; i < recvLen; i += 12) {
            int uid = Utils.Byte2Int(response, i);
            int freezeMode = Utils.Byte2Int(response, i + 4);
            int isPermissive = Utils.Byte2Int(response, i + 8);
            if (!loadedAppCfg.containsKey(uid))
                loadedPersistedUidOrder.add(uid);
            loadedAppCfg.put(uid, new Pair<>(freezeMode, isPermissive));
        }

        var cachedUidList = AppInfoCache.getUidList();
        ArrayList<Integer> uidList = new ArrayList<>(cachedUidList.size());
        HashSet<Integer> seenUids = new HashSet<>();
        for (int uid : cachedUidList) {
            if (seenUids.add(uid))
                uidList.add(uid);
        }

        // 补全  此时 uidList 可能包含一些刚刚安装的应用，而底层还没更新全部应用列表
        uidList.forEach(uid -> {
            if (!loadedAppCfg.containsKey(uid))
                loadedAppCfg.put(uid, new Pair<>(Utils.CFG_FREEZER, 1)); // 默认Freezer 宽松
        });
        ArrayList<Integer> uidListSort = new ArrayList<>(uidList.size());

        // 先排 自由
        for (int uid : uidList) {
            var mode = loadedAppCfg.get(uid);
            if (mode != null && AppConfigSerializer.normalizedFreezeMode(mode.first) == Utils.CFG_WHITELIST)
                uidListSort.add(uid);
        }

        // 优先排列：FREEZER SIGSTOP 杀死后台， 次排列：宽松 严格
        for (int uid : uidList) {
            var mode = loadedAppCfg.get(uid);
            int freezeMode = mode == null ? Utils.CFG_FREEZER : AppConfigSerializer.normalizedFreezeMode(mode.first);
            if (mode != null && (freezeMode == Utils.CFG_FREEZER || freezeMode == Utils.CFG_FREEZER_BR)
                    && mode.second != 0)
                uidListSort.add(uid);
        }
        for (int uid : uidList) {
            var mode = loadedAppCfg.get(uid);
            int freezeMode = mode == null ? Utils.CFG_FREEZER : AppConfigSerializer.normalizedFreezeMode(mode.first);
            if (mode != null && (freezeMode == Utils.CFG_FREEZER || freezeMode == Utils.CFG_FREEZER_BR)
                    && mode.second == 0)
                uidListSort.add(uid);
        }

        for (int uid : uidList) {
            var mode = loadedAppCfg.get(uid);
            int freezeMode = mode == null ? Utils.CFG_FREEZER : AppConfigSerializer.normalizedFreezeMode(mode.first);
            if (mode != null && (freezeMode == CFG_SIGSTOP || freezeMode == Utils.CFG_SIGSTOP_BR)
                    && mode.second != 0)
                uidListSort.add(uid);
        }
        for (int uid : uidList) {
            var mode = loadedAppCfg.get(uid);
            int freezeMode = mode == null ? Utils.CFG_FREEZER : AppConfigSerializer.normalizedFreezeMode(mode.first);
            if (mode != null && (freezeMode == CFG_SIGSTOP || freezeMode == Utils.CFG_SIGSTOP_BR)
                    && mode.second == 0)
                uidListSort.add(uid);
        }

        for (int uid : uidList) {
            var mode = loadedAppCfg.get(uid);
            if (mode != null && AppConfigSerializer.normalizedFreezeMode(mode.first) == Utils.CFG_TERMINATE && mode.second != 0)
                uidListSort.add(uid);
        }
        for (int uid : uidList) {
            var mode = loadedAppCfg.get(uid);
            if (mode != null && AppConfigSerializer.normalizedFreezeMode(mode.first) == Utils.CFG_TERMINATE && mode.second == 0)
                uidListSort.add(uid);
        }

        // 最后排 内置自由
        for (int uid : uidList) {
            var mode = loadedAppCfg.get(uid);
            if (mode != null && AppConfigSerializer.normalizedFreezeMode(mode.first) == Utils.CFG_WHITEFORCE)
                uidListSort.add(uid);
        }

        postConfigLoadResult(generation, new ConfigLoadSnapshot(
                uidListSort, loadedAppCfg, loadedPersistedUidOrder));
    }

    private void postConfigLoadResult(long generation, ConfigLoadSnapshot snapshot) {
        handler.obtainMessage(GET_APP_CFG, new ConfigLoadResult(generation, snapshot)).sendToTarget();
    }

    private static final class ConfigLoadResult {
        final long generation;
        final ConfigLoadSnapshot snapshot;

        ConfigLoadResult(long generation, ConfigLoadSnapshot snapshot) {
            this.generation = generation;
            this.snapshot = snapshot;
        }
    }

    private static final class ConfigLoadSnapshot {
        final List<Integer> uidList;
        final Map<Integer, Pair<Integer, Integer>> appCfg;
        final List<Integer> persistedUidOrder;

        ConfigLoadSnapshot(List<Integer> uidList,
                           Map<Integer, Pair<Integer, Integer>> appCfg,
                           List<Integer> persistedUidOrder) {
            this.uidList = Collections.unmodifiableList(new ArrayList<>(uidList));
            this.appCfg = Collections.unmodifiableMap(new HashMap<>(appCfg));
            this.persistedUidOrder = Collections.unmodifiableList(new ArrayList<>(persistedUidOrder));
        }
    }

    private final Handler handler = new Handler(Looper.getMainLooper()) {
        @SuppressLint("SetTextI18n")
        @Override
        public void handleMessage(@NonNull Message msg) {
            super.handleMessage(msg);
            if (binding == null)
                return;

            switch (msg.what) {
                case GET_APP_CFG: {
                    ConfigLoadResult result = (ConfigLoadResult) msg.obj;
                    if (result.generation != configLoadGeneration)
                        break;
                    if (result.snapshot != null) {
                        recycleAdapter.updateDataSet(result.snapshot.uidList,
                                result.snapshot.appCfg, result.snapshot.persistedUidOrder);
                        binding.fabSave.setEnabled(true);
                    }
                    binding.swipeRefreshLayout.setRefreshing(false);
                    break;
                }
                case SET_CFG_SUCCESS:
                    recycleAdapter.markSaveSuccessful((AppCfgAdapter.SaveRequest) msg.obj);
                    Toast.makeText(requireContext(), R.string.update_success, Toast.LENGTH_SHORT).show();
                    break;
                case SET_CFG_FAIL:
                    recycleAdapter.markSaveFailed((AppCfgAdapter.SaveRequest) msg.obj);
                    Toast.makeText(requireContext(), R.string.update_fail, Toast.LENGTH_SHORT).show();
                    break;
            }
        }
    };

    @Override
    public void onDestroyView() {
        ++configLoadGeneration;
        binding = null;
        super.onDestroyView();
    }

    static class AppCfgAdapter extends RecyclerView.Adapter<AppCfgAdapter.MyViewHolder> {
        ArrayList<Integer> uidList = new ArrayList<>();
        ArrayList<Integer> uidListFilter = new ArrayList<>(400);
        final HashMap<Integer, Pair<Integer, Integer>> appCfg = new HashMap<>(); //<uid, <freezeMode, permissive>>
        final HashMap<Integer, Pair<Integer, Integer>> originalAppCfg = new HashMap<>();
        final ArrayList<Integer> persistedUidOrder = new ArrayList<>();
        final HashSet<Integer> changedUids = new HashSet<>();
        final HashMap<Integer, Pair<Integer, Integer>> queuedSaveValues = new HashMap<>();
        boolean configLoaded = false;
        boolean showSystemApp = false;
        String keyWord = "";
        Context context;
        long dataRevision = 0;

        public AppCfgAdapter() {
        }

        public void setContext(Context ctx){
            context = ctx;
        }

        public void updateDataSet(@NonNull List<Integer> newUidList,
                                  @NonNull Map<Integer, Pair<Integer, Integer>> newAppCfg,
                                  @NonNull List<Integer> newPersistedUidOrder) {
            uidList = new ArrayList<>(newUidList);
            appCfg.clear();
            appCfg.putAll(newAppCfg);
            persistedUidOrder.clear();
            persistedUidOrder.addAll(newPersistedUidOrder);
            originalAppCfg.clear();
            for (int uid : persistedUidOrder) {
                Pair<Integer, Integer> cfg = appCfg.get(uid);
                if (cfg != null)
                    originalAppCfg.put(uid, cfg);
            }
            changedUids.clear();
            queuedSaveValues.clear();
            dataRevision++;
            configLoaded = true;
            keyWord = "";
            updateAndRefreshView();
        }

        void beginLoading() {
            configLoaded = false;
        }

        @NonNull
        @Override
        public MyViewHolder onCreateViewHolder(@NonNull ViewGroup parent, int viewType) {
            View view = LayoutInflater.from(parent.getContext()).
                    inflate(R.layout.app_cfg_layout, parent, false);
            return new MyViewHolder(view);
        }

        int cfgValue2idx(int i) {
            switch (i) {
                case CFG_TERMINATE:
                    return 0;
                case CFG_SIGSTOP:
                    return 1;
                case CFG_SIGSTOP_BR:
                    return 2;
                default:
                case CFG_FREEZER:
                    return 3;
                case CFG_FREEZER_BR:
                    return 4;
                case CFG_WHITELIST:
                    return 5;
            }
        }

        static int idx2cfgValue(int i) {
            switch (i) {
                case 0:
                    return CFG_TERMINATE;
                case 1:
                    return CFG_SIGSTOP;
                case 2:
                    return CFG_SIGSTOP_BR;
                default:
                case 3:
                    return CFG_FREEZER;
                case 4:
                    return CFG_FREEZER_BR;
                case 5:
                    return CFG_WHITELIST;
            }
        }

        @SuppressLint({"UseCompatLoadingForDrawables", "SetTextI18n"})
        @Override
        public void onBindViewHolder(@NonNull MyViewHolder holder, int position) {
            int uid = uidListFilter.get(position);

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

            var cfg = appCfg.get(uid);
            int freezeMode = cfg == null ? CFG_FREEZER : AppConfigSerializer.normalizedFreezeMode(cfg.first);
            int isPermissive = cfg == null ? 0 : cfg.second;

            if (freezeMode == CFG_WHITEFORCE) {
                holder.spinner_permissive.setVisibility(View.GONE);
                holder.spinner_cfg.setVisibility(View.GONE);
                return;
            }

            holder.spinner_cfg.setVisibility(View.VISIBLE);
            holder.spinner_permissive.setVisibility(freezeMode == CFG_WHITELIST ? View.GONE : View.VISIBLE);

            holder.bindingConfig = true;
            holder.spinner_cfg.setSelection(cfgValue2idx(freezeMode));
            holder.spinner_permissive.setSelection(isPermissive == 0 ? 0 : 1);
            holder.bindingConfig = false;
        }

        @Override
        public int getItemCount() {
            return uidListFilter.size();
        }

        class MyViewHolder extends RecyclerView.ViewHolder {

            ImageView app_icon;
            TextView app_label;
            Spinner spinner_cfg, spinner_permissive;
            int uid = 0;
            boolean bindingConfig = false;

            public MyViewHolder(View view) {
                super(view);
                app_icon = view.findViewById(R.id.app_icon);
                app_label = view.findViewById(R.id.app_label);
                spinner_cfg = view.findViewById(R.id.spinner_cfg);
                spinner_permissive = view.findViewById(R.id.spinner_level);

                spinner_cfg.setOnItemSelectedListener(new AdapterView.OnItemSelectedListener() {
                    @Override
                    public void onItemSelected(AdapterView<?> parent, View view, int spinnerPosition, long id) {
                        if (uid == 0 || bindingConfig) return;
                        var cfg = appCfg.get(uid);
                        int newFreezeMode = idx2cfgValue(spinnerPosition);
                        if (cfg == null || cfg.first == newFreezeMode) return;
                        updateConfig(uid, new Pair<>(newFreezeMode, cfg.second));
                        spinner_permissive.setVisibility(newFreezeMode == CFG_WHITELIST ? View.GONE : View.VISIBLE);
                    }

                    @Override
                    public void onNothingSelected(AdapterView<?> parent) {
                    }
                });

                spinner_permissive.setOnItemSelectedListener(new AdapterView.OnItemSelectedListener() {
                    @Override
                    public void onItemSelected(AdapterView<?> parent, View view, int spinnerPosition, long id) {
                        if (uid == 0 || bindingConfig) return;
                        var cfg = appCfg.get(uid);
                        if (cfg == null || cfg.second == spinnerPosition) return;
                        updateConfig(uid, new Pair<>(
                                cfg.first,
                                spinnerPosition));
                    }

                    @Override
                    public void onNothingSelected(AdapterView<?> parent) {
                    }
                });
            }

        }


        static final class SaveRequest {
            final byte[] payload;
            final Map<Integer, Pair<Integer, Integer>> values;
            final long dataRevision;

            SaveRequest(byte[] payload, Map<Integer, Pair<Integer, Integer>> values,
                        long dataRevision) {
                this.payload = payload;
                this.values = Collections.unmodifiableMap(new HashMap<>(values));
                this.dataRevision = dataRevision;
            }
        }

        public SaveRequest createSaveRequest() {
            if (!configLoaded || appCfg.isEmpty()) return null;
            Map<Integer, AppConfigSerializer.Value> values = new HashMap<>();
            Map<Integer, Pair<Integer, Integer>> savedValues = new HashMap<>();
            for (int uid : changedUids) {
                Pair<Integer, Integer> cfg = appCfg.get(uid);
                if (cfg == null)
                    continue;
                if (cfg.equals(queuedSaveValues.get(uid)))
                    continue;
                savedValues.put(uid, cfg);
                values.put(uid, new AppConfigSerializer.Value(cfg.first, cfg.second));
            }
            byte[] payload = AppConfigSerializer.encode(values, persistedUidOrder, savedValues.keySet());
            if (payload.length == 0)
                return null;
            queuedSaveValues.putAll(savedValues);
            return new SaveRequest(payload, savedValues, dataRevision);
        }

        public byte[] getCfgBytes() {
            if (!configLoaded || appCfg.isEmpty())
                return null;
            Map<Integer, AppConfigSerializer.Value> values = new HashMap<>();
            for (int uid : changedUids) {
                Pair<Integer, Integer> cfg = appCfg.get(uid);
                if (cfg != null)
                    values.put(uid, new AppConfigSerializer.Value(cfg.first, cfg.second));
            }
            byte[] payload = AppConfigSerializer.encode(values, persistedUidOrder, values.keySet());
            return payload.length == 0 ? null : payload;
        }

        void markSaveSuccessful(@NonNull SaveRequest saveRequest) {
            if (saveRequest.dataRevision != dataRevision)
                return;
            for (Map.Entry<Integer, Pair<Integer, Integer>> entry : saveRequest.values.entrySet()) {
                int uid = entry.getKey();
                Pair<Integer, Integer> savedValue = entry.getValue();
                if (savedValue.equals(queuedSaveValues.get(uid)))
                    queuedSaveValues.remove(uid);
                if (!savedValue.equals(appCfg.get(uid)))
                    continue;
                originalAppCfg.put(uid, savedValue);
                changedUids.remove(uid);
                if (!persistedUidOrder.contains(uid))
                    persistedUidOrder.add(uid);
            }
        }

        void markSaveFailed(@NonNull SaveRequest saveRequest) {
            if (saveRequest.dataRevision != dataRevision)
                return;
            for (Map.Entry<Integer, Pair<Integer, Integer>> entry : saveRequest.values.entrySet()) {
                if (entry.getValue().equals(queuedSaveValues.get(entry.getKey())))
                    queuedSaveValues.remove(entry.getKey());
            }
        }

        private void updateConfig(int uid, Pair<Integer, Integer> config) {
            appCfg.put(uid, config);
            Pair<Integer, Integer> original = originalAppCfg.get(uid);
            if (config.equals(original))
                changedUids.remove(uid);
            else
                changedUids.add(uid);
        }

        public void switchAppType() {
            showSystemApp = !showSystemApp;
            updateAndRefreshView();
            if(showSystemApp)
                Utils.textDialog(context, R.string.sys_warn_title, R.string.sys_warn_info);
        }

        public void filter(@NonNull final String _keyWord) {
            keyWord = _keyWord;
            updateAndRefreshView();
        }

        @SuppressLint("NotifyDataSetChanged")
        void updateAndRefreshView() {
            uidListFilter.clear();
            for (int uid : uidList) {
                var info = AppInfoCache.get(uid);
                if (info != null && info.isSystemApp == showSystemApp &&
                        (keyWord.isEmpty() || info.contains(keyWord)))
                    uidListFilter.add(uid);
            }
            notifyDataSetChanged();
        }
    }

}
