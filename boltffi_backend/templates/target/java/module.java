package {{ package }};
{%- for import in imports %}

{{ import }}
{%- endfor %}{% if native_class %}

final class {{ native_owner }} {
    static void ensureInitialized() {}
{{ loader }}

    private {{ native_owner }}() {}
{%- for method in native_methods %}

{{ method }}
{%- endfor %}
}
{%- endif %}
{%- for helper in helpers %}

{{ helper }}
{%- endfor %}

public final class {{ file }} {
    private {{ file }}() {}
{%- for declaration in declarations %}

{{ declaration }}
{%- endfor %}
}
