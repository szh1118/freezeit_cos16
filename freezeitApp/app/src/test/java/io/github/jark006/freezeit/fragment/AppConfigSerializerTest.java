package io.github.jark006.freezeit.fragment;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;

import org.junit.Test;

import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

public class AppConfigSerializerTest {
    @Test
    public void noOpSaveProducesNoPayload() {
        Map<Integer, AppConfigSerializer.Value> values = new HashMap<>();
        values.put(10001, new AppConfigSerializer.Value(40, 1));
        values.put(10002, new AppConfigSerializer.Value(30, 0));
        values.put(10003, new AppConfigSerializer.Value(30, 1));

        byte[] encoded = AppConfigSerializer.encode(
                values,
                List.of(10002, 10001),
                Set.of()
        );

        assertEquals(0, encoded.length);
    }

    @Test
    public void changedDisplayDefaultIsSerializedWithoutPersistedEntries() {
        Map<Integer, AppConfigSerializer.Value> values = new HashMap<>();
        values.put(10001, new AppConfigSerializer.Value(40, 1));
        values.put(10003, new AppConfigSerializer.Value(20, 0));
        Set<Integer> changed = new HashSet<>();
        changed.add(10003);

        byte[] encoded = AppConfigSerializer.encode(
                values,
                List.of(10001),
                changed
        );

        assertArrayEquals(recordBytes(10003, 20, 0), encoded);
    }

    @Test
    public void duplicatePersistedUidsAreSerializedOnceInFirstSeenOrder() {
        Map<Integer, AppConfigSerializer.Value> values = new HashMap<>();
        values.put(10001, new AppConfigSerializer.Value(40, 1));
        values.put(10002, new AppConfigSerializer.Value(30, 0));

        byte[] encoded = AppConfigSerializer.encode(
                values,
                List.of(10001, 10002, 10001),
                Set.of(10001, 10002)
        );

        assertEquals(24, encoded.length);
        assertArrayEquals(recordBytes(
                10001, 40, 1,
                10002, 30, 0
        ), encoded);
    }

    @Test
    public void unknownFreezeModeIsPreservedWhenSerialized() {
        Map<Integer, AppConfigSerializer.Value> values = new HashMap<>();
        values.put(10001, new AppConfigSerializer.Value(60, 0));

        byte[] encoded = AppConfigSerializer.encode(
                values,
                List.of(10001),
                Set.of(10001)
        );

        assertArrayEquals(recordBytes(10001, 60, 0), encoded);
    }

    @Test
    public void unchangedUnknownModeIsExcludedWhenAnotherUidChanges() {
        Map<Integer, AppConfigSerializer.Value> values = new HashMap<>();
        values.put(10001, new AppConfigSerializer.Value(60, 1));
        values.put(10002, new AppConfigSerializer.Value(20, 0));

        byte[] encoded = AppConfigSerializer.encode(
                values,
                List.of(10001, 10002),
                Set.of(10002)
        );

        assertArrayEquals(recordBytes(10002, 20, 0), encoded);
    }

    @Test
    public void emptyConfigProducesNoPayload() {
        byte[] encoded = AppConfigSerializer.encode(
                Map.of(),
                List.of(),
                Set.of()
        );

        assertEquals(0, encoded.length);
    }

    private static byte[] recordBytes(int... values) {
        byte[] bytes = new byte[values.length * 4];
        for (int index = 0; index < values.length; index++) {
            int value = values[index];
            int offset = index * 4;
            bytes[offset] = (byte) value;
            bytes[offset + 1] = (byte) (value >>> 8);
            bytes[offset + 2] = (byte) (value >>> 16);
            bytes[offset + 3] = (byte) (value >>> 24);
        }
        return bytes;
    }
}
