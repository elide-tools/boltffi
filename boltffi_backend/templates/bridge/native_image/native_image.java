package {{ contract.package() }};

import java.util.Collections;
import java.util.List;
import org.graalvm.nativeimage.CurrentIsolate;
import org.graalvm.nativeimage.Isolate;
import org.graalvm.nativeimage.PinnedObject;
import org.graalvm.nativeimage.StackValue;
import org.graalvm.nativeimage.UnmanagedMemory;
import org.graalvm.nativeimage.c.CContext;
import org.graalvm.nativeimage.c.function.CEntryPoint;
import org.graalvm.nativeimage.c.function.CEntryPointLiteral;
import org.graalvm.nativeimage.c.function.CFunction;
import org.graalvm.nativeimage.c.function.CFunctionPointer;
import org.graalvm.nativeimage.c.function.CLibrary;
import org.graalvm.nativeimage.c.function.InvokeCFunctionPointer;
import org.graalvm.nativeimage.c.struct.CField;
import org.graalvm.nativeimage.c.struct.CStruct;
import org.graalvm.nativeimage.c.struct.SizeOf;
import org.graalvm.nativeimage.c.type.CCharPointer;
import org.graalvm.nativeimage.c.type.CTypeConversion;
import org.graalvm.word.PointerBase;
import org.graalvm.word.WordFactory;

@CContext({{ contract.class_name() }}.Directives.class)
@CLibrary(value = "{{ contract.library() }}", requireStatic = true)
final class {{ contract.class_name() }} {
    private {{ contract.class_name() }}() {}
{% include "bridge/native_image/support.java" %}
{% include "bridge/native_image/lib.java" %}
{%- for wrapper in wrappers %}

{{ wrapper }}
{%- endfor %}
{%- for trampoline in trampolines %}

{{ trampoline }}
{%- endfor %}
}
