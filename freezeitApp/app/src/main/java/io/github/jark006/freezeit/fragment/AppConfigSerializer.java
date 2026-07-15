package io.github.jark006.freezeit.fragment;

import io.github.jark006.freezeit.Utils;

import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

final class AppConfigSerializer {
    static final class Value {
        final int freezeMode;
        final int permissive;

        Value(int freezeMode, int permissive) {
            this.freezeMode = freezeMode;
            this.permissive = permissive;
        }
    }

    private AppConfigSerializer() {
    }

    static byte[] encode(Map<Integer, Value> values,
                         List<Integer> persistedUidOrder,
                         Set<Integer> changedUids) {
        ArrayList<Integer> outputOrder = new ArrayList<>();
        HashSet<Integer> included = new HashSet<>();
        for (int uid : persistedUidOrder) {
            if (changedUids.contains(uid) && values.containsKey(uid) && included.add(uid)) {
                outputOrder.add(uid);
            }
        }

        ArrayList<Integer> newUids = new ArrayList<>();
        for (int uid : changedUids) {
            if (values.containsKey(uid) && included.add(uid)) {
                newUids.add(uid);
            }
        }
        Collections.sort(newUids);
        outputOrder.addAll(newUids);

        byte[] bytes = new byte[outputOrder.size() * 12];
        int offset = 0;
        for (int uid : outputOrder) {
            Value value = values.get(uid);
            writeInt(uid, bytes, offset);
            writeInt(value.freezeMode, bytes, offset + 4);
            writeInt(value.permissive, bytes, offset + 8);
            offset += 12;
        }
        return bytes;
    }

    static int normalizedFreezeMode(int freezeMode) {
        return Utils.CFG_SET.contains(freezeMode) ? freezeMode : Utils.CFG_FREEZER;
    }

    private static void writeInt(int value, byte[] bytes, int offset) {
        bytes[offset] = (byte) value;
        bytes[offset + 1] = (byte) (value >>> 8);
        bytes[offset + 2] = (byte) (value >>> 16);
        bytes[offset + 3] = (byte) (value >>> 24);
    }
}
