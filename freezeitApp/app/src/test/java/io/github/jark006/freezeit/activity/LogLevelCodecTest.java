package io.github.jark006.freezeit.activity;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public class LogLevelCodecTest {
    @Test
    public void storageValuesMapToFixedSpinnerOrder() {
        assertEquals(0, LogLevelCodec.toSpinnerPosition(0));
        assertEquals(4, LogLevelCodec.toSpinnerPosition(1));
        assertEquals(1, LogLevelCodec.toSpinnerPosition(2));
        assertEquals(2, LogLevelCodec.toSpinnerPosition(3));
        assertEquals(3, LogLevelCodec.toSpinnerPosition(4));
        assertEquals(0, LogLevelCodec.toSpinnerPosition(255));
    }

    @Test
    public void spinnerOrderMapsBackToStableStorageValues() {
        assertEquals(0, LogLevelCodec.toStorageValue(0));
        assertEquals(2, LogLevelCodec.toStorageValue(1));
        assertEquals(3, LogLevelCodec.toStorageValue(2));
        assertEquals(4, LogLevelCodec.toStorageValue(3));
        assertEquals(1, LogLevelCodec.toStorageValue(4));
        assertEquals(0, LogLevelCodec.toStorageValue(-1));
        assertEquals(0, LogLevelCodec.toStorageValue(5));
    }
}
