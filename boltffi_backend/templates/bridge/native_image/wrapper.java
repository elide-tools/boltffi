    static {{ wrapper.returns() }} {{ wrapper.name() }}({{ wrapper.declarations() }}) {
{%- for line in wrapper.body() %}
        {{ line }}
{%- endfor %}
    }
