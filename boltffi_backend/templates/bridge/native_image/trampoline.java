    @CEntryPoint(name = "{{ trampoline.entry_symbol() }}")
    @com.oracle.svm.guest.staging.c.function.CEntryPointOptions(prologue = AttachPrologue.class{% match trampoline.prologue_bailout() %}{% when Some with (bailout) %}, prologueBailout = {{ bailout }}.class{% when None %}{% endmatch %})
    static {{ trampoline.java_returns() }} {{ trampoline.java_name() }}(HandleCell __cell{% if !trampoline.java_params().is_empty() %}, {{ trampoline.java_params() }}{% endif %}) {
{%- for line in trampoline.body() %}
        {{ line }}
{%- endfor %}
    }
