
    /** Direct declarations of the C ABI (or its pointer-ABI shims). */
    static final class Lib {
        private Lib() {}
{%- for downcall in contract.downcalls() %}
{%- if downcall.declares_lib() %}

        @CFunction{% if downcall.no_transition() %}(transition = CFunction.Transition.NO_TRANSITION){% endif %}
        static native {{ downcall.lib_returns() }} {{ downcall.lib_name() }}({{ downcall.lib_declarations() }});
{%- endif %}
{%- endfor %}
    }
