package io.github.jark006.freezeit.activity;

import android.Manifest;
import android.annotation.SuppressLint;
import android.content.ActivityNotFoundException;
import android.content.ContentResolver;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.graphics.ImageDecoder;
import android.graphics.Rect;
import android.graphics.drawable.BitmapDrawable;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Message;
import android.os.SystemClock;
import android.util.Log;
import android.view.View;
import android.widget.AdapterView;
import android.widget.SeekBar;
import android.widget.Spinner;
import android.widget.Switch;
import android.widget.TextView;
import android.widget.Toast;

import androidx.activity.result.ActivityResultLauncher;
import androidx.activity.result.contract.ActivityResultContracts;
import androidx.appcompat.app.AppCompatActivity;
import androidx.core.app.ActivityCompat;

import io.github.jark006.freezeit.ManagerCmd;
import io.github.jark006.freezeit.R;
import io.github.jark006.freezeit.StaticData;
import io.github.jark006.freezeit.Utils;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.StandardCopyOption;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;


public class Settings extends AppCompatActivity implements View.OnClickListener {
    private static final String TAG = "Freezeit[Settings]";
    private static final int MAX_BACKGROUND_WIDTH = 1080;
    private static final ExecutorService SETTINGS_RPC_EXECUTOR = Executors.newSingleThreadExecutor();
    private static final ExecutorService BACKGROUND_IMPORT_EXECUTOR = Executors.newSingleThreadExecutor();
    private static final Handler MAIN_HANDLER = new Handler(Looper.getMainLooper());

    final int INIT_UI = 1, SET_VAR_SUCCESS = 2, SET_VAR_FAIL = 3, GET_SETTINGS_FAIL = 4;

    Spinner freezeModeSpinner, reFreezeTimeoutSpinner, wakeupTimeoutSpinner, logLevelSpinner;
    SeekBar freezeTimeoutSeekbar, terminateTimeoutSeekbar;
    TextView freezeTimeoutValueText, terminateTimeoutValueText;

    @SuppressLint("UseSwitchCompatOrMaterialCode")
    Switch batterySwitch, currentSwitch,  doubleCellSwitch,
            lmkSwitch, dozeSwitch;

    final int freezeTimeoutIdx = 2;
    final int wakeupTimeoutIdx = 3;
    final int terminateTimeoutIdx = 4;
    final int freezeModeIdx = 5;
    final int reFreezeTimeoutIdx = 6;

    final int batteryIdx = 13;
    final int currentIdx = 14;
    final int doubleCellIdx = 15;
    final int lmkIdx = 16;
    final int dozeIdx = 17;

    final int debugIdx = 30;

    byte[] settingsVar = new byte[256];
    long lastTimestamp = 0;
    long settingsRevision = 0;
    long nextSetOperation = 0;
    final long[] latestSetOperation = new long[256];

    ActivityResultLauncher<Intent> pickPicture;

    private static final class SettingsSnapshot {
        final long revision;
        final byte[] values;

        SettingsSnapshot(long revision, byte[] values) {
            this.revision = revision;
            this.values = values;
        }
    }

    private static final class SetVarResult {
        final int index;
        final int value;
        final long operation;
        final String error;

        SetVarResult(int index, int value, long operation, String error) {
            this.index = index;
            this.value = value;
            this.operation = operation;
            this.error = error;
        }
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_settings);

        findViewById(R.id.freeze_mode_title).setOnClickListener(this);
        findViewById(R.id.freeze_timeout_title).setOnClickListener(this);
        findViewById(R.id.refreeze_timeout_title).setOnClickListener(this);
        findViewById(R.id.terminate_timeout_title).setOnClickListener(this);
        findViewById(R.id.wakeup_timeout_title).setOnClickListener(this);
        findViewById(R.id.battery_title).setOnClickListener(this);
        findViewById(R.id.current_title).setOnClickListener(this);
        findViewById(R.id.double_cell_title).setOnClickListener(this);
        findViewById(R.id.lmk_title).setOnClickListener(this);
        findViewById(R.id.doze_title).setOnClickListener(this);
        findViewById(R.id.log_level_title).setOnClickListener(this);

        findViewById(R.id.set_bg).setOnClickListener(this);

        freezeModeSpinner = findViewById(R.id.freeze_mode_spinner);
        reFreezeTimeoutSpinner = findViewById(R.id.refreeze_timeout_spinner);
        wakeupTimeoutSpinner = findViewById(R.id.wakeup_timeout_spinner);
        logLevelSpinner = findViewById(R.id.log_level_spinner);

        freezeTimeoutValueText = findViewById(R.id.freeze_timeout_value_text);
        terminateTimeoutValueText = findViewById(R.id.terminate_timeout_value_text);

        freezeTimeoutSeekbar = findViewById(R.id.seekBarTimeout);
        terminateTimeoutSeekbar = findViewById(R.id.seekBarTerminate);

        batterySwitch = findViewById(R.id.switch_battery);
        currentSwitch = findViewById(R.id.switch_current);
        doubleCellSwitch = findViewById(R.id.switch_double_cell);
        lmkSwitch = findViewById(R.id.switch_lmk);
        dozeSwitch = findViewById(R.id.switch_doze);

        if (this.checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) !=
                PackageManager.PERMISSION_GRANTED) {
            ActivityCompat.requestPermissions(Settings.this,
                    new String[]{Manifest.permission.WRITE_EXTERNAL_STORAGE}, 1);
        }

        pickPicture = registerForActivityResult(new ActivityResultContracts.StartActivityForResult(), result -> {
            if (result.getResultCode() != RESULT_OK || result.getData() == null ||
                    result.getData().getData() == null)
                return;

            final Uri imageUri = result.getData().getData();
            final Context appContext = getApplicationContext();
            BACKGROUND_IMPORT_EXECUTOR.execute(() -> importBackground(appContext, imageUri));
        });
    }

    @Override
    public void onResume() {
        super.onResume();
        findViewById(R.id.container).setBackground(StaticData.getBackgroundDrawable(this));
        final long revision = ++settingsRevision;
        SETTINGS_RPC_EXECUTOR.execute(() -> {
            Utils.TaskResult result = Utils.freezeitTaskResult(ManagerCmd.getSettings, null);
            if (result.length() != 256) {
                handler.sendMessage(Message.obtain(handler, GET_SETTINGS_FAIL,
                        Long.valueOf(revision)));
                return;
            }
            handler.sendMessage(Message.obtain(handler, INIT_UI,
                    new SettingsSnapshot(revision, result.payload())));
        });
    }


    void InitSpinner(Spinner spinner, int idx) {
        spinner.setSelection(settingsVar[idx]);

        spinner.setOnItemSelectedListener(new AdapterView.OnItemSelectedListener() {
            @Override
            public void onItemSelected(AdapterView<?> parent, View view, int spinnerPosition, long id) {
                if (settingsVar[idx] == spinnerPosition)
                    return;

                var now = SystemClock.elapsedRealtime();
                if ((now - lastTimestamp) < 1000) {
                    Toast.makeText(getBaseContext(), getString(R.string.slowly_tips), Toast.LENGTH_LONG).show();
                    spinner.setSelection(settingsVar[idx]);
                    return;
                }
                lastTimestamp = now;

                setVarTask(idx, spinnerPosition);
            }

            @Override
            public void onNothingSelected(AdapterView<?> parent) {
            }
        });
    }

    void InitLogLevelSpinner() {
        logLevelSpinner.setSelection(
                LogLevelCodec.toSpinnerPosition(Byte.toUnsignedInt(settingsVar[debugIdx])));

        logLevelSpinner.setOnItemSelectedListener(new AdapterView.OnItemSelectedListener() {
            @Override
            public void onItemSelected(AdapterView<?> parent, View view, int spinnerPosition, long id) {
                int storageValue = LogLevelCodec.toStorageValue(spinnerPosition);
                if (Byte.toUnsignedInt(settingsVar[debugIdx]) == storageValue)
                    return;

                var now = SystemClock.elapsedRealtime();
                if ((now - lastTimestamp) < 1000) {
                    Toast.makeText(getBaseContext(), getString(R.string.slowly_tips), Toast.LENGTH_LONG).show();
                    logLevelSpinner.setSelection(
                            LogLevelCodec.toSpinnerPosition(Byte.toUnsignedInt(settingsVar[debugIdx])));
                    return;
                }
                lastTimestamp = now;

                setVarTask(debugIdx, storageValue);
            }

            @Override
            public void onNothingSelected(AdapterView<?> parent) {
            }
        });
    }

    void InitSeekbar(SeekBar seekBar, TextView textView, int idx) {
        seekBar.setProgress(settingsVar[idx]);
        textView.setText(String.valueOf(settingsVar[idx]));

        seekBar.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            @Override
            public void onProgressChanged(SeekBar seekBar, int progress, boolean fromUser) {
                textView.setText(String.valueOf(seekBar.getProgress()));
            }

            @Override
            public void onStartTrackingTouch(SeekBar seekBar) {
            }

            @Override
            public void onStopTrackingTouch(SeekBar seekBar) {
                if (settingsVar[idx] == seekBar.getProgress())
                    return;

                var now = SystemClock.elapsedRealtime();
                if ((now - lastTimestamp) < 1000) {
                    Toast.makeText(getBaseContext(), getString(R.string.slowly_tips), Toast.LENGTH_SHORT).show();
                    seekBar.setProgress(settingsVar[idx]);//进度条，文字 恢复原值
                    textView.setText(String.valueOf(settingsVar[idx]));
                    return;
                }
                lastTimestamp = now;

                setVarTask(idx, seekBar.getProgress());
            }
        });
    }

    void InitSwitch(@SuppressLint("UseSwitchCompatOrMaterialCode") Switch sw, int idx) {
        sw.setChecked(settingsVar[idx] != 0);

        sw.setOnCheckedChangeListener((buttonView, isChecked) -> {
            if (settingsVar[idx] == (isChecked ? 1 : 0))
                return;

            var now = SystemClock.elapsedRealtime();
            if ((now - lastTimestamp) < 1000) {
                Toast.makeText(getBaseContext(), getString(R.string.slowly_tips), Toast.LENGTH_LONG).show();
                sw.setChecked(settingsVar[idx] != 0);
                return;
            }
            lastTimestamp = now;

            setVarTask(idx, isChecked ? 1 : 0);
        });
    }

    void setVarTask(int index, int value) {
        final long operation = ++nextSetOperation;
        latestSetOperation[index] = operation;
        ++settingsRevision;
        SETTINGS_RPC_EXECUTOR.execute(() -> {
            byte[] request = {(byte) index, (byte) value};
            Utils.TaskResult result = Utils.freezeitTaskResult(ManagerCmd.setSettingsVar, request);

            if (result.length() == 0) {
                handler.sendMessage(Message.obtain(handler, SET_VAR_FAIL,
                        new SetVarResult(index, value, operation, "UnknownError")));
            } else {
                String res = new String(result.payload());
                if (res.equals("success")) {
                    handler.sendMessage(Message.obtain(handler, SET_VAR_SUCCESS,
                            new SetVarResult(index, value, operation, null)));
                } else {
                    handler.sendMessage(Message.obtain(handler, SET_VAR_FAIL,
                            new SetVarResult(index, value, operation, res)));
                }
            }
        });
    }

    private final Handler handler = new Handler(Looper.getMainLooper()) {
        @Override
        public void handleMessage(Message msg) {
            switch (msg.what) {
                case INIT_UI:
                    SettingsSnapshot snapshot = (SettingsSnapshot) msg.obj;
                    if (snapshot.revision != settingsRevision)
                        break;
                    System.arraycopy(snapshot.values, 0, settingsVar, 0, settingsVar.length);
                    InitSpinner(freezeModeSpinner, freezeModeIdx);
                    InitSpinner(reFreezeTimeoutSpinner, reFreezeTimeoutIdx);
                    InitSpinner(wakeupTimeoutSpinner, wakeupTimeoutIdx);

                    InitSeekbar(freezeTimeoutSeekbar, freezeTimeoutValueText, freezeTimeoutIdx);
                    InitSeekbar(terminateTimeoutSeekbar, terminateTimeoutValueText, terminateTimeoutIdx);

                    InitSwitch(batterySwitch, batteryIdx);
                    InitSwitch(currentSwitch, currentIdx);
                    InitSwitch(doubleCellSwitch, doubleCellIdx);
                    InitSwitch(lmkSwitch, lmkIdx);
                    InitSwitch(dozeSwitch, dozeIdx);
                    InitLogLevelSpinner();
                    break;

                case SET_VAR_SUCCESS:
                    SetVarResult success = (SetVarResult) msg.obj;
                    settingsVar[success.index] = (byte) success.value;
                    break;

                case SET_VAR_FAIL:
                    SetVarResult failure = (SetVarResult) msg.obj;
                    if (latestSetOperation[failure.index] == failure.operation) {
                        if (failure.index == debugIdx) {
                            logLevelSpinner.setSelection(
                                    LogLevelCodec.toSpinnerPosition(Byte.toUnsignedInt(settingsVar[debugIdx])));
                        } else {
                            restoreSettingControl(failure.index);
                        }
                    }
                    Toast.makeText(getBaseContext(), getString(R.string.setup_failed) + ": " + failure.error,
                            Toast.LENGTH_LONG).show();
                    break;

                case GET_SETTINGS_FAIL:
                    if (((Long) msg.obj) == settingsRevision) {
                        Toast.makeText(getBaseContext(), getString(R.string.get_settings_fail),
                                Toast.LENGTH_LONG).show();
                    }
                    break;
            }
        }
    };

    private static Bitmap decodeBackground(Context context, Uri imageUri) throws IOException {
        ImageDecoder.Source source;
        if (ContentResolver.SCHEME_FILE.equals(imageUri.getScheme())) {
            String path = imageUri.getPath();
            if (path == null)
                throw new IOException("Image URI has no file path");
            source = ImageDecoder.createSource(new File(path));
        } else {
            source = ImageDecoder.createSource(context.getContentResolver(), imageUri);
        }

        return ImageDecoder.decodeBitmap(source, (decoder, info, ignored) -> {
            int sourceWidth = info.getSize().getWidth();
            int sourceHeight = info.getSize().getHeight();
            int cropWidth = Math.min(sourceWidth, sourceHeight / 2);
            if (cropWidth <= 0)
                throw new IllegalArgumentException("Image is too small");

            int cropHeight = cropWidth * 2;
            int left = (sourceWidth - cropWidth) / 2;
            int top = (sourceHeight - cropHeight) / 2;
            int targetWidth = Math.min(cropWidth, MAX_BACKGROUND_WIDTH);
            decoder.setCrop(new Rect(left, top, left + cropWidth, top + cropHeight));
            decoder.setTargetSize(targetWidth, targetWidth * 2);
            decoder.setAllocator(ImageDecoder.ALLOCATOR_SOFTWARE);
        });
    }

    private static void replaceBackgroundFile(File temporary, File target) throws IOException {
        try {
            Files.move(temporary.toPath(), target.toPath(),
                    StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING);
        } catch (AtomicMoveNotSupportedException ignored) {
            Files.move(temporary.toPath(), target.toPath(), StandardCopyOption.REPLACE_EXISTING);
        }
    }

    private static void importBackground(Context context, Uri imageUri) {
        File temporary = null;
        Bitmap background = null;
        try {
            background = decodeBackground(context, imageUri);
            if (background == null || background.getWidth() == 0 || background.getHeight() == 0)
                throw new IOException("Image decode failed");

            temporary = File.createTempFile("bg-", ".jpg", context.getFilesDir());
            try (FileOutputStream output = new FileOutputStream(temporary)) {
                if (!background.compress(Bitmap.CompressFormat.JPEG, 90, output))
                    throw new IOException("Image compression failed");
                output.flush();
                output.getFD().sync();
            }

            replaceBackgroundFile(temporary,
                    new File(context.getFilesDir(), StaticData.bgFileName));
            final Bitmap importedBackground = background;
            if (!MAIN_HANDLER.post(() -> {
                StaticData.bg = new BitmapDrawable(context.getResources(), importedBackground);
                StaticData.bg.setAlpha(56);
            })) {
                throw new IllegalStateException("Main handler is unavailable");
            }
            background = null;
        } catch (Exception | OutOfMemoryError e) {
            Log.e(TAG, "Import background failed", e);
            MAIN_HANDLER.post(() -> Toast.makeText(context, R.string.update_fail,
                    Toast.LENGTH_LONG).show());
        } finally {
            if (temporary != null && temporary.exists() && !temporary.delete())
                Log.w(TAG, "Could not delete temporary background image");
            if (background != null && !background.isRecycled())
                background.recycle();
        }
    }

    private void restoreSettingControl(int index) {
        switch (index) {
            case freezeModeIdx:
                freezeModeSpinner.setSelection(Byte.toUnsignedInt(settingsVar[index]));
                break;
            case reFreezeTimeoutIdx:
                reFreezeTimeoutSpinner.setSelection(Byte.toUnsignedInt(settingsVar[index]));
                break;
            case wakeupTimeoutIdx:
                wakeupTimeoutSpinner.setSelection(Byte.toUnsignedInt(settingsVar[index]));
                break;
            case freezeTimeoutIdx:
                freezeTimeoutSeekbar.setProgress(Byte.toUnsignedInt(settingsVar[index]));
                freezeTimeoutValueText.setText(String.valueOf(Byte.toUnsignedInt(settingsVar[index])));
                break;
            case terminateTimeoutIdx:
                terminateTimeoutSeekbar.setProgress(Byte.toUnsignedInt(settingsVar[index]));
                terminateTimeoutValueText.setText(String.valueOf(Byte.toUnsignedInt(settingsVar[index])));
                break;
            case batteryIdx:
                batterySwitch.setChecked(settingsVar[index] != 0);
                break;
            case currentIdx:
                currentSwitch.setChecked(settingsVar[index] != 0);
                break;
            case doubleCellIdx:
                doubleCellSwitch.setChecked(settingsVar[index] != 0);
                break;
            case lmkIdx:
                lmkSwitch.setChecked(settingsVar[index] != 0);
                break;
            case dozeIdx:
                dozeSwitch.setChecked(settingsVar[index] != 0);
                break;
            case debugIdx:
                logLevelSpinner.setSelection(
                        LogLevelCodec.toSpinnerPosition(Byte.toUnsignedInt(settingsVar[index])));
                break;
        }
    }

    @Override
    public void onClick(View v) {
        int id = v.getId();
        if (id == R.id.freeze_mode_title) {
            Utils.textDialog(this, R.string.freeze_mode_title, R.string.freeze_mode_tips);
        } else if (id == R.id.freeze_timeout_title) {
            Utils.textDialog(this, R.string.freeze_timeout_title, R.string.freeze_timeout_tips);
        } else if (id == R.id.refreeze_timeout_title) {
            Utils.textDialog(this, R.string.refreeze_timeout_title, R.string.refreeze_timeout_tips);
        } else if (id == R.id.terminate_timeout_title) {
            Utils.textDialog(this, R.string.terminate_timeout_title, R.string.terminate_timeout_tips);
        } else if (id == R.id.wakeup_timeout_title) {
            Utils.textDialog(this, R.string.wakeup_timeout_title, R.string.wakeup_timeout_tips);
        } else if (id == R.id.battery_title) {
            Utils.textDialog(this, R.string.battery_title, R.string.battery_tips);
        } else if (id == R.id.current_title) {
            Utils.textDialog(this, R.string.current_title, R.string.current_tips);
        } else if (id == R.id.double_cell_title) {
            Utils.textDialog(this, R.string.double_cell_title, R.string.double_cell_tips);
        } else if (id == R.id.lmk_title) {
            Utils.textDialog(this, R.string.lmk_title, R.string.lmk_tips);
        } else if (id == R.id.doze_title) {
            Utils.textDialog(this, R.string.doze_title, R.string.doze_tips);
        } else if (id == R.id.log_level_title) {
            Utils.textDialog(this, R.string.log_level_title, R.string.log_level_tips);
        } else if (id == R.id.set_bg) {
            Intent intent = new Intent(Intent.ACTION_GET_CONTENT);
            intent.setType("image/*");
            if (intent.resolveActivity(getPackageManager()) == null) {
                Toast.makeText(this, R.string.update_fail, Toast.LENGTH_LONG).show();
                return;
            }
            try {
                pickPicture.launch(intent);
            } catch (ActivityNotFoundException e) {
                Toast.makeText(this, R.string.update_fail, Toast.LENGTH_LONG).show();
            }
        }
    }
}
