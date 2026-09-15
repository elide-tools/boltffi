
    /** Build-time directives: the generated C header and its include directory. */
    static final class Directives implements CContext.Directives {
        @Override
        public List<String> getHeaderFiles() {
            return Collections.singletonList("<{{ contract.header_file() }}>");
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
