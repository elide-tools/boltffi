
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
