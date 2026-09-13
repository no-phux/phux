# Spec identifier registries. POSIX awk: no gawk extensions or Python needed.
# Tables own their ID namespace, except ID/Direction/Name frame catalogs, which
# also share one wire namespace across files. Repeated cross-references may name
# the same frame; two rows in one table may never claim the same ID.

function clean(value) {
    gsub(/`/, "", value)
    sub(/^[[:space:]]+/, "", value)
    sub(/[[:space:]]+$/, "", value)
    return value
}

function hex(value,    number, i) {
    value = tolower(value)
    sub(/^0x/, "", value)
    number = 0
    for (i = 1; i <= length(value); i++)
        number = 16 * number + index("0123456789abcdef", substr(value, i, 1)) - 1
    return number
}

function reserve(scope, id, name, allow_reference,    key, location) {
    key = scope SUBSEP id
    location = FILENAME ":" FNR
    if (key in names && (!allow_reference || names[key] != name))
        print location ": duplicate ID " id " for " name " in " scope "; first claimed by " names[key] " at " locations[key]
    else {
        names[key] = name
        locations[key] = location
    }
}

function table_row(line,    cells, id, name) {
    split(line, cells, "|")
    id = clean(cells[2])
    if (id !~ /^0[xX][0-9a-fA-F]+$/) return
    name = clean(cells[3])
    if (frame_catalog) name = clean(cells[4])
    reserve(table, hex(id), name, 0)
    if (frame_catalog) reserve("message", hex(id), name, 1)
}

function bitset_row(line,    fields, name, value) {
    split(line, fields, "=")
    name = clean(fields[1])
    value = fields[2]
    sub(/,.*/, "", value)
    value = clean(value)
    if (value ~ /^0[xX][0-9a-fA-F]+$/) {
        reserve(bitset, hex(value), name, 0)
        return
    }
    if (value ~ /^1[[:space:]]*<<[[:space:]]*[0-9]+$/) {
        sub(/^1[[:space:]]*<<[[:space:]]*/, "", value)
        reserve(bitset, 2 ^ value, name, 0)
    }
}

FNR == 1 { table = ""; bitset = ""; fenced = 0 }
/^```/ { fenced = !fenced; bitset = ""; next }
!fenced && /^\|[[:space:]]*(ID|Tag|Code|Bit)[[:space:]]*\|/ {
    table = FILENAME ":" FNR
    frame_catalog = ($0 ~ /\|[[:space:]]*Direction[[:space:]]*\|/)
    next
}
!fenced && /^\|/ && table != "" { table_row($0); next }
!fenced && !/^\|/ { table = "" }
fenced && /^[A-Za-z][A-Za-z0-9_]* = bitset / {
    bitset = FILENAME ":" $1
    next
}
fenced && /^}/ { bitset = "" }
bitset != "" && /^[[:space:]]*[A-Za-z][A-Za-z0-9_]*[[:space:]]*=/ { bitset_row($0) }
