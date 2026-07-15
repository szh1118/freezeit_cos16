package io.github.jark006.freezeit;

import android.app.AlertDialog;
import android.app.Dialog;
import android.content.ContentResolver;
import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.Matrix;
import android.net.Uri;
import android.util.Log;
import android.widget.ImageView;

import androidx.annotation.DrawableRes;
import androidx.annotation.LayoutRes;
import androidx.annotation.NonNull;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.net.URL;
import java.net.URLConnection;
import java.util.Set;

public class Utils {
    public final static int CFG_TERMINATE = 10,
            CFG_SIGSTOP = 20,
            CFG_SIGSTOP_BR = 21, //附加断网
            CFG_FREEZER = 30,
            CFG_FREEZER_BR = 31, //附加断网
            CFG_WHITELIST = 40,
            CFG_WHITEFORCE = 50;
    public final static Set<Integer> CFG_SET = Set.of(
            CFG_TERMINATE,
            CFG_SIGSTOP,
            CFG_SIGSTOP_BR,
            CFG_FREEZER,
            CFG_FREEZER_BR,
            CFG_WHITELIST,
            CFG_WHITEFORCE
    );

    private static final String TAG = "Freezeit[Utils]";
    private static final int MAX_FRAME_PAYLOAD_BYTES = 1024 * 1024;
    private static final int SOCKET_CONNECT_TIMEOUT_MS = 3_000;
    private static final int SOCKET_TOTAL_DEADLINE_MS = 5_000;
    private static final int NETWORK_TIMEOUT_MS = 5_000;

    public static final class TaskResult {
        private final byte[] payload;
        private final boolean success;

        private TaskResult(byte[] payload) {
            this(payload, false);
        }

        private TaskResult(byte[] payload, boolean success) {
            this.payload = payload;
            this.success = success;
        }

        public int length() {
            return payload.length;
        }

        public byte[] payload() {
            return payload;
        }

        // 区分「守护进程返回了合法的空 payload」（例如 ClearLog 成功后日志为空，应清屏）
        // 与「RPC 失败」（连接超时/校验错误，应保留旧内容并提示）。两者 length() 都是 0，
        // 必须用 success 区分，否则清屏操作会被误报为失败、或失败时把旧日志清空。
        public boolean success() {
            return success;
        }
    }

    public static TaskResult freezeitTaskResult(byte command, byte[] AdditionalData) {
        // dataHeader[0-3]:附带数据大小(uint32 小端)
        // dataHeader[4]: 命令(可参考上面)
        // dataHeader[5]: 附带数据的异或校验值
        if (AdditionalData != null && AdditionalData.length > MAX_FRAME_PAYLOAD_BYTES) {
            Log.e(TAG, "Request payload is too large: " + AdditionalData.length);
            return new TaskResult(new byte[0]);
        }

        byte[] dataHeader = {0, 0, 0, 0, command, 0};
        long deadlineNanos = System.nanoTime() + SOCKET_TOTAL_DEADLINE_MS * 1_000_000L;
        try (Socket socket = new Socket()) {
            socket.connect(new InetSocketAddress("127.0.0.1", 60613),
                    Math.min(SOCKET_CONNECT_TIMEOUT_MS, remainingTimeoutMillis(deadlineNanos)));
            InputStream is = socket.getInputStream();
            OutputStream os = socket.getOutputStream();

            if (AdditionalData != null && AdditionalData.length > 0) {
                Int2Byte(AdditionalData.length, dataHeader, 0);
                dataHeader[5] = checksum(AdditionalData);

                os.write(dataHeader);
                os.write(AdditionalData);
            } else {
                os.write(dataHeader);
            }

            os.flush();

            if (!readFully(socket, is, dataHeader, 0, 6, deadlineNanos)) {
                Log.e(TAG, "Receive dataHeader Fail");
                return new TaskResult(new byte[0]);
            }

            if (dataHeader[4] != command) {
                Log.e(TAG, "Unexpected response command: " + Byte.toUnsignedInt(dataHeader[4]));
                return new TaskResult(new byte[0]);
            }

            final int payloadLen = Byte2Int(dataHeader, 0);
            // 必须设上限，否则恶意/畸形响应头 payloadLen=0x7FFFFFFF 会通过 <0 检查并
            // 触发 new byte[2147483647] 抛 OutOfMemoryError（catch 只接 IOException）。
            // 与 Rust 端 MAX_PAYLOAD_LEN (1 MiB) 对齐。
            if (payloadLen < 0 || payloadLen > MAX_FRAME_PAYLOAD_BYTES) {
                Log.e(TAG, "Invalid payloadLen:" + payloadLen);
                return new TaskResult(new byte[0]);
            }
            byte[] response = new byte[payloadLen];
            if (!readFully(socket, is, response, 0, payloadLen, deadlineNanos)) {
                Log.e(TAG, "Get payload Fail");
                return new TaskResult(new byte[0]);
            }
            if (dataHeader[5] != checksum(response)) {
                Log.e(TAG, "Response checksum mismatch");
                return new TaskResult(new byte[0]);
            }

            return new TaskResult(response, true);
        } catch (IOException e) {
            return new TaskResult(new byte[0]);
        }
    }

    private static byte checksum(byte[] payload) {
        byte checksum = 0;
        for (byte value : payload)
            checksum ^= value;
        return checksum;
    }

    private static int remainingTimeoutMillis(long deadlineNanos) throws SocketTimeoutException {
        long remainingNanos = deadlineNanos - System.nanoTime();
        if (remainingNanos <= 0)
            throw new SocketTimeoutException("Manager response deadline exceeded");

        return (int) Math.max(1L, (remainingNanos + 999_999L) / 1_000_000L);
    }

    private static boolean readFully(Socket socket, InputStream is, byte[] bytes, int offset, int length,
                                     long deadlineNanos) throws IOException {
        int readCnt = 0;
        while (readCnt < length) {
            socket.setSoTimeout(remainingTimeoutMillis(deadlineNanos));
            int cnt = is.read(bytes, offset + readCnt, length - readCnt);
            if (cnt <= 0)
                return false;
            readCnt += cnt;
            if (System.nanoTime() >= deadlineNanos)
                throw new SocketTimeoutException("Manager response deadline exceeded");
        }
        return true;
    }

    public static byte[] getNetworkData(String link) {
        HttpURLConnection conn = null;
        try {
            URL url = new URL(link);
            URLConnection connection = url.openConnection();
            if (!(connection instanceof HttpURLConnection))
                return null;

            conn = (HttpURLConnection) connection;
            conn.setConnectTimeout(NETWORK_TIMEOUT_MS);
            conn.setReadTimeout(NETWORK_TIMEOUT_MS);
            conn.setRequestMethod("GET");
            if (conn.getResponseCode() != HttpURLConnection.HTTP_OK)
                return null;

            try (InputStream is = conn.getInputStream();
                 ByteArrayOutputStream os = new ByteArrayOutputStream()) {
                int total = 0;
                int len;
                byte[] buffer = new byte[4096];
                while ((len = is.read(buffer)) != -1) {
                    if (len > MAX_FRAME_PAYLOAD_BYTES - total) {
                        Log.w(TAG, "Network response is too large");
                        return null;
                    }
                    os.write(buffer, 0, len);
                    total += len;
                }
                return os.toByteArray();
            }
        } catch (IOException e) {
            return null;
        } finally {
            if (conn != null)
                conn.disconnect();
        }
    }

    public static void imgDialog(Context context, @DrawableRes int drawableID) {
        Dialog dialog = new Dialog(context);
        dialog.setContentView(R.layout.img_dialog);
        ((ImageView) dialog.findViewById(R.id.img)).setImageResource(drawableID);
        dialog.show();
    }

    public static void layoutDialog(Context context, @LayoutRes int layoutId) {
        Dialog dialog = new Dialog(context);
        dialog.setContentView(layoutId);
        dialog.show();
    }

    public static void textDialog(Context context, int titleResID, int contentResID) {
        AlertDialog.Builder builder = new AlertDialog.Builder(context);
        builder.setTitle(titleResID).setMessage(contentResID).create().show();
    }

    public static Bitmap resize(Bitmap bitmap, float scale) {
        Matrix matrix = new Matrix();
        matrix.postScale(scale, scale);
        return Bitmap.createBitmap(bitmap, 0, 0, bitmap.getWidth(), bitmap.getHeight(), matrix, false);
    }

    // 小端 只是避免内存越界，不处理转换失败的情况
    public static int Byte2Int(byte[] bytes, int byteOffset) {
        if (bytes == null || (byteOffset + 4) > bytes.length)
            return 0;

        return Byte.toUnsignedInt(bytes[byteOffset]) |
                (Byte.toUnsignedInt(bytes[byteOffset + 1]) << 8) |
                (Byte.toUnsignedInt(bytes[byteOffset + 2]) << 16) |
                (Byte.toUnsignedInt(bytes[byteOffset + 3]) << 24);
    }

    public static void Int2Byte(int value, byte[] bytes, int byteOffset) {
        if (bytes == null) return;
        if ((byteOffset + 4) > bytes.length) {
            while (byteOffset < bytes.length)
                bytes[byteOffset++] = 0;
            return;
        }

        bytes[byteOffset++] = (byte) value;
        bytes[byteOffset++] = (byte) (value >> 8);
        bytes[byteOffset++] = (byte) (value >> 16);
        bytes[byteOffset] = (byte) (value >> 24);
    }

    public static void Byte2Int(byte[] bytes, int byteOffset, int byteLength, int[] ints, int intOffset) {
        if (ints == null || bytes == null || (intOffset + byteLength / 4) > ints.length ||
                (byteOffset + byteLength) > bytes.length)
            return;

        for (int byteIdx = byteOffset; byteIdx < byteOffset + byteLength; byteIdx += 4) {
            ints[intOffset++] = Byte.toUnsignedInt(bytes[byteIdx]) |
                    (Byte.toUnsignedInt(bytes[byteIdx + 1]) << 8) |
                    (Byte.toUnsignedInt(bytes[byteIdx + 2]) << 16) |
                    (Byte.toUnsignedInt(bytes[byteIdx + 3]) << 24);
        }
    }

    public static void Int2Byte(int[] ints, int intOffset, int intLength, byte[] bytes, int byteOffset) {
        if (ints == null || bytes == null || (intOffset + intLength) > ints.length ||
                (byteOffset + intLength * 4) > bytes.length)
            return;

        for (int intIdx = intOffset; intIdx < intOffset + intLength; intIdx++) {
            bytes[byteOffset++] = (byte) ints[intIdx];
            bytes[byteOffset++] = (byte) (ints[intIdx] >> 8);
            bytes[byteOffset++] = (byte) (ints[intIdx] >> 16);
            bytes[byteOffset++] = (byte) (ints[intIdx] >> 24);
        }
    }

    public static String getFileAbsolutePath(@NonNull Context context, @NonNull Uri uri) {
        if (ContentResolver.SCHEME_FILE.equals(uri.getScheme())) {
            String path = uri.getPath();
            return path == null ? null : new File(path).getAbsolutePath();
        }
        if (!ContentResolver.SCHEME_CONTENT.equals(uri.getScheme()))
            return null;

        File cache = null;
        boolean copied = false;
        try {
            File cacheDirectory = context.getCacheDir();
            if (cacheDirectory == null)
                return null;

            cache = File.createTempFile("freezeit-", ".tmp", cacheDirectory);
            try (InputStream input = context.getContentResolver().openInputStream(uri);
                 FileOutputStream output = new FileOutputStream(cache)) {
                if (input == null)
                    throw new IOException("Content provider returned no stream");

                byte[] buffer = new byte[8192];
                int length;
                while ((length = input.read(buffer)) != -1)
                    output.write(buffer, 0, length);
                output.flush();
            }
            copied = true;
            return cache.getAbsolutePath();
        } catch (IOException | SecurityException e) {
            Log.e(TAG, "Copy content URI failed", e);
            return null;
        } finally {
            if (!copied && cache != null && cache.exists() && !cache.delete())
                Log.w(TAG, "Could not delete partial content cache file");
        }
    }
}
