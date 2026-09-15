
    /** Build-time directives: the generated C header and its include directory. */
    static final class Directives implements CContext.Directives {
        @Override
        public List<String> getHeaderFiles() {
            return java.util.Arrays.asList("<{{ contract.header_file() }}>", "<{{ contract.glue_header_file() }}>");
        }

        @Override
        public List<String> getOptions() {
            return Collections.singletonList("-I" + System.getProperty("boltffi.include", "."));
        }
    }

    /** Owned BoltFFI byte buffer (`FfiBuf_u8`) viewed through a pointer. */
    @CStruct("FfiBuf_u8")
    interface FfiBuf extends PointerBase {
        @CField("ptr") CCharPointer ptr();
        @CField("ptr") void ptr(CCharPointer value);
        @CField("len") long len();
        @CField("len") void len(long value);
        @CField("cap") void cap(long value);
        @CField("align") void align(long value);
    }
{%- for record in contract.direct_records() %}

    /** Size probe for the direct record `{{ record }}`. */
    @CStruct("{{ record }}")
    interface {{ record }}Struct extends PointerBase {}
{%- endfor %}

    /** Unmanaged cell behind a Java-owned callback handle. */
    @CStruct("BoltFFINativeImageHandleCell")
    interface HandleCell extends PointerBase {
        @CField("isolate") Isolate isolate();
        @CField("isolate") void isolate(Isolate value);
        @CField("token") long token();
        @CField("token") void token(long value);
    }

    /** `BoltFFICallbackHandle` viewed through a pointer. */
    @CStruct("BoltFFICallbackHandle")
    interface CallbackHandle extends PointerBase {
        @CField("handle") long handle();
        @CField("handle") void handle(long value);
        @CField("vtable") PointerBase vtable();
        @CField("vtable") void vtable(PointerBase value);
    }

    interface FreeSlot extends CFunctionPointer {
        @InvokeCFunctionPointer void invoke(long handle);
    }

    interface CloneSlot extends CFunctionPointer {
        @InvokeCFunctionPointer long invoke(long handle);
    }

    /** Leading free and clone slots of every callback vtable. */
    @CStruct("BoltFFINativeImageVTablePrefix")
    interface VTablePrefix extends PointerBase {
        @CField("free") FreeSlot free();
        @CField("clone") CloneSlot cloneSlot();
    }

    /** Enters the isolate stored in the handle cell, attaching foreign threads on first use. */
    static final class AttachPrologue implements com.oracle.svm.guest.staging.c.function.CEntryPointOptions.Prologue {
        @com.oracle.svm.shared.Uninterruptible(reason = "Entry point prologue runs before the isolate is entered.")
        static int enter(HandleCell cell) {
            return com.oracle.svm.guest.staging.c.function.CEntryPointActions.enterAttachThread(cell.isolate(), true);
        }
    }
{%- if contract.registers_callbacks() %}

    static {
        Lib.{{ contract.register_callbacks() }}();
    }
{%- endif %}

    /** Allocates the unmanaged cell Rust receives as a Java-owned callback handle. */
    static long newHandleCell(long token) {
        HandleCell cell = UnmanagedMemory.malloc(SizeOf.get(HandleCell.class));
        cell.isolate(CurrentIsolate.getIsolate());
        cell.token(token);
        return cell.rawValue();
    }

    /** Copies a Rust-owned callback handle into unmanaged memory and returns its address as the Java token. */
    static long storeOwnedHandle(CallbackHandle value) {
        if (value.handle() == 0L || value.vtable().isNull()) {
            return 0L;
        }
        CallbackHandle stored = UnmanagedMemory.malloc(SizeOf.get(CallbackHandle.class));
        stored.handle(value.handle());
        stored.vtable(value.vtable());
        return stored.rawValue();
    }

    /** Resolves a Rust-owned callback handle token into the stored `BoltFFICallbackHandle`. */
    static CallbackHandle ownedHandle(long token) {
        CallbackHandle stored = WordFactory.pointer(token);
        if (stored.isNull() || stored.handle() == 0L || stored.vtable().isNull()) {
            throw new RuntimeException("BoltFFI callback handle was null or invalid");
        }
        return stored;
    }

    static long cloneOwnedHandle(long token) {
        CallbackHandle stored = WordFactory.pointer(token);
        if (stored.isNull() || stored.handle() == 0L || stored.vtable().isNull()) {
            return 0L;
        }
        VTablePrefix prefix = (VTablePrefix) stored.vtable();
        long cloned = prefix.cloneSlot().invoke(stored.handle());
        if (cloned == 0L) {
            return 0L;
        }
        CallbackHandle copy = UnmanagedMemory.malloc(SizeOf.get(CallbackHandle.class));
        copy.handle(cloned);
        copy.vtable(stored.vtable());
        return copy.rawValue();
    }

    static void releaseOwnedHandle(long token) {
        CallbackHandle stored = WordFactory.pointer(token);
        if (stored.isNull()) {
            return;
        }
        if (stored.handle() != 0L && stored.vtable().isNonNull()) {
            ((VTablePrefix) stored.vtable()).free().invoke(stored.handle());
        }
        UnmanagedMemory.free(stored);
    }
{%- for callback in contract.callbacks() %}

    /** Converts a host token for `{{ callback.dispatch_class() }}` into a `BoltFFICallbackHandle`. */
    static void {{ callback.parameter_helper() }}(long token, CallbackHandle out) {
        if ((token & 1L) != 0L) {
            Lib.{{ callback.create_handle_lib() }}(newHandleCell(token), out);
            return;
        }
        CallbackHandle stored = WordFactory.pointer(token);
        if (stored.isNull() || stored.handle() == 0L || stored.vtable().isNull()) {
            out.handle(0L);
            out.vtable(WordFactory.nullPointer());
            return;
        }
        out.handle(((VTablePrefix) stored.vtable()).cloneSlot().invoke(stored.handle()));
        out.vtable(stored.vtable());
    }
{%- endfor %}

    static void zeroCallbackHandle(CallbackHandle handle) {
        handle.handle(0L);
        handle.vtable(WordFactory.nullPointer());
    }

    static void zeroBuffer(FfiBuf buffer) {
        buffer.ptr(WordFactory.nullPointer());
        buffer.len(0L);
        buffer.cap(0L);
        buffer.align(0L);
    }

    /** Returns the base address of a direct buffer with at least `required` capacity. */
    static long directAddress(java.nio.ByteBuffer buffer, long required) {
        if (buffer == null) {
            throw new IllegalArgumentException("BoltFFI direct buffer argument was null");
        }
        if (required < 0) {
            throw new IllegalArgumentException("BoltFFI direct buffer length was negative");
        }
        if (!buffer.isDirect()) {
            throw new IllegalArgumentException("BoltFFI argument was not a direct buffer");
        }
        if (buffer.capacity() < required) {
            throw new IllegalArgumentException("BoltFFI direct buffer capacity was too small");
        }
        return java.lang.foreign.MemorySegment.ofBuffer(buffer.duplicate().clear()).address();
    }

    /** Copies an owned Rust buffer into a Java array and releases it. */
    static byte[] takeBuffer(FfiBuf buffer) {
        long length = buffer.len();
        if (buffer.ptr().isNull()) {
            if (length != 0) {
                throw new RuntimeException("BoltFFI buffer pointer was null with non-zero length");
            }
            return new byte[0];
        }
        if (length > Integer.MAX_VALUE) {
            Lib.{{ contract.free_buffer() }}(buffer);
            throw new RuntimeException("BoltFFI buffer too large for Java byte array");
        }
        byte[] bytes = new byte[(int) length];
        CTypeConversion.asByteBuffer(buffer.ptr(), (int) length).get(bytes);
        Lib.{{ contract.free_buffer() }}(buffer);
        return bytes;
    }

    static void zeroMemory(PointerBase pointer, int length) {
        CTypeConversion.asByteBuffer(pointer, length).put(new byte[length]);
    }

    static byte[] copyBytes(PointerBase pointer, long length) {
        if (length > Integer.MAX_VALUE) {
            throw new RuntimeException("BoltFFI byte slice too large for Java array");
        }
        byte[] bytes = new byte[(int) length];
        if (length != 0) {
            CTypeConversion.asByteBuffer(pointer, (int) length).get(bytes);
        }
        return bytes;
    }

    static boolean[] copyBooleans(PointerBase pointer, long length) {
        byte[] bytes = copyBytes(pointer, length);
        boolean[] values = new boolean[bytes.length];
        for (int i = 0; i < bytes.length; i++) values[i] = bytes[i] != 0;
        return values;
    }

    static short[] copyShorts(PointerBase pointer, long length) {
        short[] values = new short[checkedLength(length, 2)];
        if (length != 0) CTypeConversion.asByteBuffer(pointer, values.length * 2).order(java.nio.ByteOrder.nativeOrder()).asShortBuffer().get(values);
        return values;
    }

    static int[] copyInts(PointerBase pointer, long length) {
        int[] values = new int[checkedLength(length, 4)];
        if (length != 0) CTypeConversion.asByteBuffer(pointer, values.length * 4).order(java.nio.ByteOrder.nativeOrder()).asIntBuffer().get(values);
        return values;
    }

    static long[] copyLongs(PointerBase pointer, long length) {
        long[] values = new long[checkedLength(length, 8)];
        if (length != 0) CTypeConversion.asByteBuffer(pointer, values.length * 8).order(java.nio.ByteOrder.nativeOrder()).asLongBuffer().get(values);
        return values;
    }

    static float[] copyFloats(PointerBase pointer, long length) {
        float[] values = new float[checkedLength(length, 4)];
        if (length != 0) CTypeConversion.asByteBuffer(pointer, values.length * 4).order(java.nio.ByteOrder.nativeOrder()).asFloatBuffer().get(values);
        return values;
    }

    static double[] copyDoubles(PointerBase pointer, long length) {
        double[] values = new double[checkedLength(length, 8)];
        if (length != 0) CTypeConversion.asByteBuffer(pointer, values.length * 8).order(java.nio.ByteOrder.nativeOrder()).asDoubleBuffer().get(values);
        return values;
    }

    private static int checkedLength(long length, int width) {
        if (length < 0 || length > Integer.MAX_VALUE / width) {
            throw new RuntimeException("BoltFFI vector argument too large for Java array");
        }
        return (int) length;
    }

    /** Returns the byte length of a stream batch buffer holding `maxCount` items. */
    static int checkedBatchBytes(long maxCount, int itemSize) {
        if (maxCount < 0) {
            throw new IllegalArgumentException("BoltFFI stream batch size was negative");
        }
        if (maxCount > Integer.MAX_VALUE / itemSize) {
            throw new RuntimeException("BoltFFI stream batch allocation is too large");
        }
        return (int) maxCount * itemSize;
    }

    static byte[] copyRecord(PointerBase pointer, int size) {
        return copyBytes(pointer, size);
    }

    static void checkRecordLength(byte[] value, int size) {
        if (value == null || value.length != size) {
            throw new IllegalArgumentException("BoltFFI record byte array length did not match the C record size");
        }
    }

    static void writeRecord(PointerBase pointer, byte[] value, int size) {
        if (value == null || value.length != size) {
            throw new IllegalArgumentException("BoltFFI record byte array length did not match the C record size");
        }
        CTypeConversion.asByteBuffer(pointer, size).put(value);
    }

    /** Copies Java bytes into a Rust-owned buffer at `out`. */
    static void writeBuffer(FfiBuf out, byte[] value) {
        if (value == null) {
            throw new RuntimeException("BoltFFI byte array return was null");
        }
        if (value.length == 0) {
            zeroBuffer(out);
            return;
        }
        try (PinnedObject pin = PinnedObject.create(value)) {
            Lib.{{ contract.buffer_from_bytes() }}(pin.addressOfArrayElement(0), (long) value.length, out);
        }
    }

    static void checkStatus(int code) {
        if (code != 0) {
            throw new RuntimeException("BoltFFI call failed");
        }
    }

    static void checkErrorBuffer(FfiBuf error) {
        if (error.ptr().isNonNull() || error.len() != 0) {
            throw new BoltFfiErrorBufferException(takeBuffer(error));
        }
    }
