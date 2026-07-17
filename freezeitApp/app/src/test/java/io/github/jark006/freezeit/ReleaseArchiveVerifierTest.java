package io.github.jark006.freezeit;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.io.OutputStream;

import org.junit.Test;

public class ReleaseArchiveVerifierTest {
    @Test
    public void copiesOnlyWhenTheDownloadedArchiveMatchesItsExpectedDigest() throws Exception {
        byte[] archive = "safe archive".getBytes(StandardCharsets.UTF_8);
        ByteArrayOutputStream output = new ByteArrayOutputStream();

        assertTrue(ReleaseArchiveVerifier.copyAndVerify(
                new ByteArrayInputStream(archive), output,
                "3235d9c7f811a4af79c2d775bfbe230977967318c96bbf2d2c81c9ae12b5a383"));
        assertArrayEquals(archive, output.toByteArray());

        output.reset();
        assertFalse(ReleaseArchiveVerifier.copyAndVerify(
                new ByteArrayInputStream(archive), output,
                "0000000000000000000000000000000000000000000000000000000000000000"));
    }

    @Test
    public void acceptsExactlyTheMaximumArchiveSize() throws Exception {
        String expected = digestFor(ReleaseArchiveVerifier.MAX_ARCHIVE_BYTES);
        CountingOutputStream output = new CountingOutputStream();

        assertTrue(ReleaseArchiveVerifier.copyAndVerify(
                new SizedInputStream(ReleaseArchiveVerifier.MAX_ARCHIVE_BYTES), output, expected));
        assertEquals(ReleaseArchiveVerifier.MAX_ARCHIVE_BYTES, output.bytesWritten);
    }

    @Test
    public void rejectsAnArchiveLargerThanTheMaximum() throws Exception {
        CountingOutputStream output = new CountingOutputStream();

        assertFalse(ReleaseArchiveVerifier.copyAndVerify(
                new SizedInputStream(ReleaseArchiveVerifier.MAX_ARCHIVE_BYTES + 1), output,
                "0000000000000000000000000000000000000000000000000000000000000000"));
        assertEquals(ReleaseArchiveVerifier.MAX_ARCHIVE_BYTES, output.bytesWritten);
    }

    @Test(expected = IOException.class)
    public void propagatesReadFailuresForTheCallerToCleanUp() throws Exception {
        ReleaseArchiveVerifier.copyAndVerify(new InputStream() {
            @Override
            public int read(byte[] buffer, int offset, int length) throws IOException {
                throw new IOException("read failed");
            }

            @Override
            public int read() throws IOException {
                throw new IOException("read failed");
            }
        }, new CountingOutputStream(),
                "0000000000000000000000000000000000000000000000000000000000000000");
    }

    private static String digestFor(long bytes) throws NoSuchAlgorithmException {
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        byte[] buffer = new byte[16 * 1024];
        long remaining = bytes;
        while (remaining > 0) {
            int length = (int) Math.min(buffer.length, remaining);
            digest.update(buffer, 0, length);
            remaining -= length;
        }
        StringBuilder result = new StringBuilder(64);
        for (byte value : digest.digest())
            result.append(String.format("%02x", value & 0xff));
        return result.toString();
    }

    private static final class SizedInputStream extends InputStream {
        private long remaining;

        SizedInputStream(long remaining) {
            this.remaining = remaining;
        }

        @Override
        public int read(byte[] buffer, int offset, int length) {
            if (remaining == 0)
                return -1;
            int count = (int) Math.min(length, remaining);
            java.util.Arrays.fill(buffer, offset, offset + count, (byte) 0);
            remaining -= count;
            return count;
        }

        @Override
        public int read() {
            if (remaining == 0)
                return -1;
            remaining--;
            return 0;
        }
    }

    private static final class CountingOutputStream extends OutputStream {
        long bytesWritten;

        @Override
        public void write(int value) {
            bytesWritten++;
        }

        @Override
        public void write(byte[] buffer, int offset, int length) {
            bytesWritten += length;
        }
    }
}
