package io.github.jark006.freezeit.activity;

final class LogLevelCodec {
    private static final int[] STORAGE_BY_POSITION = {0, 2, 3, 4, 1};

    private LogLevelCodec() {
    }

    static int toSpinnerPosition(int storageValue) {
        for (int position = 0; position < STORAGE_BY_POSITION.length; position++) {
            if (STORAGE_BY_POSITION[position] == storageValue) {
                return position;
            }
        }
        return 0;
    }

    static int toStorageValue(int position) {
        if (position < 0 || position >= STORAGE_BY_POSITION.length) {
            return 0;
        }
        return STORAGE_BY_POSITION[position];
    }
}
