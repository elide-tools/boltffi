    static {{ downcall.wrapper_returns() }} {{ downcall.wrapper_name() }}({{ downcall.wrapper_declarations() }}) {
{%- if self.scalar() %}
        {% if !downcall.wrapper_returns_void() %}return {% endif %}Lib.{{ downcall.lib_name() }}({{ downcall.wrapper_arguments() }});
{%- endif %}
    }
