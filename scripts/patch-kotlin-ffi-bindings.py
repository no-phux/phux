#!/usr/bin/env python3
"""Patch Kotlin/JVM-only naming AND semantics gaps in UniFFI's generated
Kotlin bindings (phux-nng, phux-80k). Run by the Android artifact producer
right after uniffi-bindgen generates the file — never hand-edit the generated
file itself.

Two naming collisions are Rust names that are completely reasonable for Rust
and Swift and simply happen to be reserved-ish in Kotlin/JVM:

1. RemoteClientInterface.close() collides with AutoCloseable.close().
   Renamed to stopConnection() for Kotlin only.

2. WireException.Runtime's `message` field collides with Throwable.message.
   Renamed to `reason` for Kotlin only.

A THIRD fix is semantic: UniFFI's AutoCloseable.close() only destroy()s the
native pointer. Disposal must stop the wire session first.

Each replacement asserts its expected occurrence count before writing.
Pass the generated .kt path as argv[1].
"""
import sys
from pathlib import Path


def replace_exact(text: str, old: str, new: str, expected: int, label: str) -> str:
    count = text.count(old)
    if count != expected:
        sys.exit(
            f"patch-kotlin-ffi-bindings: expected {expected} occurrence(s) of "
            f"{label!r}, found {count} -- uniffi-bindgen's output shape may "
            "have changed; update this script rather than skipping it."
        )
    return text.replace(old, new)


def replace_in_class(
    text: str, class_marker: str, old: str, new: str, label: str
) -> str:
    start = text.find(class_marker)
    if start == -1:
        sys.exit(
            f"patch-kotlin-ffi-bindings: class marker {class_marker!r} not "
            "found -- uniffi-bindgen's output shape may have changed."
        )
    next_class = text.find("\nopen class ", start + len(class_marker))
    end = next_class if next_class != -1 else len(text)
    scoped = text[start:end]
    count = scoped.count(old)
    if count != 1:
        sys.exit(
            f"patch-kotlin-ffi-bindings: expected exactly 1 occurrence of "
            f"{label!r} within {class_marker!r}'s body, found {count}."
        )
    patched_scope = scoped.replace(old, new, 1)
    return text[:start] + patched_scope + text[end:]


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(f"usage: {Path(sys.argv[0]).name} BINDINGS.kt")
    bindings_path = Path(sys.argv[1])
    text = bindings_path.read_text()

    text = replace_exact(
        text, "fun `close`()", "fun `stopConnection`()", 2, "fun `close`()"
    )

    text = replace_in_class(
        text,
        "open class RemoteClient:",
        "    @Synchronized\n    override fun close() {\n        this.destroy()\n    }",
        "    @Synchronized\n    override fun close() {\n"
        "        try {\n"
        "            this.stopConnection()\n"
        "        } finally {\n"
        "            this.destroy()\n"
        "        }\n"
        "    }",
        "RemoteClient AutoCloseable.close() body",
    )

    text = replace_exact(
        text,
        "    class Runtime(\n        \n        val `message`: kotlin.String\n"
        '        ) : WireException() {\n        override val message\n'
        '            get() = "message=${ `message` }"\n    }',
        "    class Runtime(\n        \n        val `reason`: kotlin.String\n"
        '        ) : WireException() {\n        override val message\n'
        '            get() = "message=${ `reason` }"\n    }',
        1,
        "WireException.Runtime class body",
    )
    text = replace_exact(
        text,
        "            is WireException.Runtime -> (\n"
        "                // Add the size for the Int that specifies the variant plus the size needed for all fields\n"
        "                4UL\n"
        "                + FfiConverterString.allocationSize(value.`message`)\n"
        "            )",
        "            is WireException.Runtime -> (\n"
        "                // Add the size for the Int that specifies the variant plus the size needed for all fields\n"
        "                4UL\n"
        "                + FfiConverterString.allocationSize(value.`reason`)\n"
        "            )",
        1,
        "WireException.Runtime allocationSize",
    )
    text = replace_exact(
        text,
        "            is WireException.Runtime -> {\n"
        "                buf.putInt(2)\n"
        "                FfiConverterString.write(value.`message`, buf)\n"
        "                Unit\n"
        "            }",
        "            is WireException.Runtime -> {\n"
        "                buf.putInt(2)\n"
        "                FfiConverterString.write(value.`reason`, buf)\n"
        "                Unit\n"
        "            }",
        1,
        "WireException.Runtime write",
    )

    bindings_path.write_text(text)
    print(f"patch-kotlin-ffi-bindings: patched {bindings_path}")


if __name__ == "__main__":
    main()
