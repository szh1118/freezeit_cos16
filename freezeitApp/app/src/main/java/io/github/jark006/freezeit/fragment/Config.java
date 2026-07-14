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
import java.util.HashMap;
import java.util.HashSet;
import java.util.Locale;
import java.util.Map;

import io.github.jark006.freezeit.AppInfoCache;
import io.github.jark006.freezeit.ManagerCmd;
import io.github.jark006.freezeit.R;
import io.github.jark006.freezeit.Utils;
import io.github.jark006.freezeit.databinding.FragmentConfigBinding;

public class Config extends Fragment {
    private final static String TAG = "ConfigFragment";
    final int GET_APP_CFG = 1,
            SET_CFG_SUCCESS = 2,
            SET_CFG_FAIL = 3;

    private FragmentConfigBinding binding;
    AppCfgAdapter recycleAdapter = new AppCfgAdapter();
    long lastTimestamp = 0;


    // 配置名单 <uid, <freezeMode, isPermissive>>
    // freezeMode: [10]:杀死 [20]:SIGSTOP [21]:SIGSTOP断网 [30]:Freezer [31]:Freezer断网 [40]:自由 [50]:内置
    HashMap<Integer, Pair<Integer, Integer>> appCfg = new HashMap<>();
    ArrayList<Integer> uidListSort = new ArrayList<>();
    ArrayList<Integer> persistedUidOrder = new ArrayList<>();

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

        binding.swipeRefreshLayout.setOnRefreshListener(() -> {
            beginConfigLoad();
            // 在主线程取 context 再传入后台线程；若在线程体内调 requireContext()，
            // Fragment detach（onDetach 置空 mContext）后会抛 IllegalStateException 崩溃。
            final android.content.Context context = requireContext().getApplicationContext();
            new Thread(() -> {
                AppInfoCache.refreshCache(context);// 下拉刷新时，先更新应用缓存
                getAppCfgTask();
            }).start();
        });

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
            var now = System.currentTimeMillis();
            if ((now - lastTimestamp) < 1000) {
                Toast.makeText(requireContext(), getString(R.string.slowly_tips), Toast.LENGTH_LONG).show();
                return;
            }
            lastTimestamp = now;

            byte[] newConf = recycleAdapter.getCfgBytes();
            if (newConf == null || newConf.length == 0) return;

            new Thread(() -> {
                Utils.TaskResult result = Utils.freezeitTaskResult(ManagerCmd.setAppCfg, newConf);
                handler.sendEmptyMessage((result.length() == 7 &&
                        new String(result.payload()).equals("success")) ?
                        SET_CFG_SUCCESS : SET_CFG_FAIL);
            }).start();
        });

        binding.fabSwitchSys.setOnClickListener(view -> {
            var now = System.currentTimeMillis();
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

        beginConfigLoad();
        new Thread(this::getAppCfgTask).start();
    }

    private void beginConfigLoad() {
        recycleAdapter.beginLoading();
        if (binding != null) {
            binding.fabSave.setEnabled(false);
            binding.swipeRefreshLayout.setRefreshing(true);
        }
    }

    void getAppCfgTask() {
        Utils.TaskResult result = Utils.freezeitTaskResult(ManagerCmd.getAppCfg, null);
        int recvLen = result.length();
        if (recvLen == 0 || recvLen % 12 != 0) {
            handler.post(() -> {
                if (binding != null)
                    binding.swipeRefreshLayout.setRefreshing(false);
            });
            return;
        }
        byte[] response = result.payload();

        appCfg.clear();
        persistedUidOrder.clear();
        // 每个配置含：3个[int32]数据，12字节 小端
        for (int i = 0; i < recvLen; i += 12) {
            int uid = Utils.Byte2Int(response, i);
            int freezeMode = Utils.Byte2Int(response, i + 4);
            int isPermissive = Utils.Byte2Int(response, i + 8);
            if (!appCfg.containsKey(uid))
                persistedUidOrder.add(uid);
            appCfg.put(uid, new Pair<>(freezeMode, isPermissive));
        }

        var uidList = AppInfoCache.getUidList();
        // 补全  此时 uidList 可能包含一些刚刚安装的应用，而底层还没更新全部应用列表
        uidList.forEach(uid -> {
            if (!appCfg.containsKey(uid))
                appCfg.put(uid, new Pair<>(Utils.CFG_FREEZER, 1)); // 默认Freezer 宽松
        });
        uidListSort.clear();

        // 先排 自由
        for (int uid : uidList) {
            var mode = appCfg.get(uid);
            if (mode != null && AppConfigSerializer.normalizedFreezeMode(mode.first) == Utils.CFG_WHITELIST)
                uidListSort.add(uid);
        }

        // 优先排列：FREEZER SIGSTOP 杀死后台， 次排列：宽松 严格
        for (int uid : uidList) {
            var mode = appCfg.get(uid);
            int freezeMode = mode == null ? Utils.CFG_FREEZER : AppConfigSerializer.normalizedFreezeMode(mode.first);
            if (mode != null && (freezeMode == Utils.CFG_FREEZER || freezeMode == Utils.CFG_FREEZER_BR)
                    && mode.second != 0)
                uidListSort.add(uid);
        }
        for (int uid : uidList) {
            var mode = appCfg.get(uid);
            int freezeMode = mode == null ? Utils.CFG_FREEZER : AppConfigSerializer.normalizedFreezeMode(mode.first);
            if (mode != null && (freezeMode == Utils.CFG_FREEZER || freezeMode == Utils.CFG_FREEZER_BR)
                    && mode.second == 0)
                uidListSort.add(uid);
        }

        for (int uid : uidList) {
            var mode = appCfg.get(uid);
            int freezeMode = mode == null ? Utils.CFG_FREEZER : AppConfigSerializer.normalizedFreezeMode(mode.first);
            if (mode != null && (freezeMode == CFG_SIGSTOP || freezeMode == Utils.CFG_SIGSTOP_BR)
                    && mode.second != 0)
                uidListSort.add(uid);
        }
        for (int uid : uidList) {
            var mode = appCfg.get(uid);
            int freezeMode = mode == null ? Utils.CFG_FREEZER : AppConfigSerializer.normalizedFreezeMode(mode.first);
            if (mode != null && (freezeMode == CFG_SIGSTOP || freezeMode == Utils.CFG_SIGSTOP_BR)
                    && mode.second == 0)
                uidListSort.add(uid);
        }

        for (int uid : uidList) {
            var mode = appCfg.get(uid);
            if (mode != null && AppConfigSerializer.normalizedFreezeMode(mode.first) == Utils.CFG_TERMINATE && mode.second != 0)
                uidListSort.add(uid);
        }
        for (int uid : uidList) {
            var mode = appCfg.get(uid);
            if (mode != null && AppConfigSerializer.normalizedFreezeMode(mode.first) == Utils.CFG_TERMINATE && mode.second == 0)
                uidListSort.add(uid);
        }

        // 最后排 内置自由
        for (int uid : uidList) {
            var mode = appCfg.get(uid);
            if (mode != null && AppConfigSerializer.normalizedFreezeMode(mode.first) == Utils.CFG_WHITEFORCE)
                uidListSort.add(uid);
        }

        handler.sendEmptyMessage(GET_APP_CFG);
    }

    private final Handler handler = new Handler(Looper.getMainLooper()) {
        @SuppressLint("SetTextI18n")
        @Override
        public void handleMessage(@NonNull Message msg) {
            super.handleMessage(msg);
            if (binding == null)
                return;

            switch (msg.what) {
                case GET_APP_CFG:
                    recycleAdapter.updateDataSet(uidListSort, appCfg, persistedUidOrder);
                    binding.fabSave.setEnabled(true);
                    binding.swipeRefreshLayout.setRefreshing(false);
                    break;
                case SET_CFG_SUCCESS:
                    Toast.makeText(requireContext(), R.string.update_success, Toast.LENGTH_SHORT).show();
                    break;
                case SET_CFG_FAIL:
                    Toast.makeText(requireContext(), R.string.update_fail, Toast.LENGTH_SHORT).show();
                    break;
            }
        }
    };

    @Override
    public void onDestroyView() {
        super.onDestroyView();
        binding = null;
    }


    static class AppCfgAdapter extends RecyclerView.Adapter<AppCfgAdapter.MyViewHolder> {
        ArrayList<Integer> uidList = new ArrayList<>();
        ArrayList<Integer> uidListFilter = new ArrayList<>(400);
        final HashMap<Integer, Pair<Integer, Integer>> appCfg = new HashMap<>(); //<uid, <freezeMode, permissive>>
        final HashMap<Integer, Pair<Integer, Integer>> originalAppCfg = new HashMap<>();
        final ArrayList<Integer> persistedUidOrder = new ArrayList<>();
        final HashSet<Integer> changedUids = new HashSet<>();
        boolean configLoaded = false;
        boolean showSystemApp = false;
        String keyWord = "";
        Context context;

        public AppCfgAdapter() {
        }

        public void setContext(Context ctx){
            context = ctx;
        }

        public void updateDataSet(@NonNull ArrayList<Integer> newUidList,
                                  @NonNull HashMap<Integer, Pair<Integer, Integer>> newAppCfg,
                                  @NonNull ArrayList<Integer> newPersistedUidOrder) {
            uidList = newUidList;
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
                                AppConfigSerializer.normalizedFreezeMode(cfg.first),
                                spinnerPosition));
                    }

                    @Override
                    public void onNothingSelected(AdapterView<?> parent) {
                    }
                });
            }

        }


        public byte[] getCfgBytes() {
            if (!configLoaded || appCfg.isEmpty()) return null;
            Map<Integer, AppConfigSerializer.Value> values = new HashMap<>();
            appCfg.forEach((uid, cfg) ->
                    values.put(uid, new AppConfigSerializer.Value(cfg.first, cfg.second)));
            byte[] payload = AppConfigSerializer.encode(values, persistedUidOrder, changedUids);
            return payload.length == 0 ? null : payload;
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
