package io.github.jark006.freezeit;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.Locale;

/** Verifies an update archive before the caller hands it to a privileged installer. */
public final class ReleaseArchiveVerifier {
    public static final long MAX_ARCHIVE_BYTES = 64L * 1024L * 1024L;

    private ReleaseArchiveVerifier() {
    }

    public static boolean isValidSha256(String expected) {
        return expected != null && expected.matches("[0-9a-fA-F]{64}");
    }

    public static boolean copyAndVerify(InputStream input, OutputStream output, String expected)
            throws IOException {
        if (!isValidSha256(expected))
            return false;

        final MessageDigest digest;
        try {
            digest = MessageDigest.getInstance("SHA-256");
        } catch (NoSuchAlgorithmException e) {
            throw new AssertionError("Android must provide SHA-256", e);
        }

        byte[] buffer = new byte[16 * 1024];
        long total = 0;
        int length;
        while ((length = input.read(buffer)) != -1) {
            if (length > MAX_ARCHIVE_BYTES - total)
                return false;
            digest.update(buffer, 0, length);
            output.write(buffer, 0, length);
            total += length;
        }

        String actual = toHex(digest.digest());
        return MessageDigest.isEqual(
                actual.getBytes(java.nio.charset.StandardCharsets.US_ASCII),
                expected.toLowerCase(Locale.ROOT).getBytes(java.nio.charset.StandardCharsets.US_ASCII));
    }

    private static String toHex(byte[] bytes) {
        char[] digits = "0123456789abcdef".toCharArray();
        char[] result = new char[bytes.length * 2];
        for (int index = 0; index < bytes.length; index++) {
            int value = bytes[index] & 0xff;
            result[index * 2] = digits[value >>> 4];
            result[index * 2 + 1] = digits[value & 0x0f];
        }
        return new String(result);
    }
}
