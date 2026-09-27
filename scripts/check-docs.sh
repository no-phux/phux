#!/usr/bin/env bash
# Mechanical enforcement of docs/CONVENTIONS.md. The gate table in
# CONVENTIONS.md §"CI enforcement" says what each gate catches.
#
# Usage: bash scripts/check-docs.sh [--help] [--list] [--only=<gate>]
# Exit codes: 0 no violations, 1 violations, 2 bad invocation.
#
# Runs on the Bash 3.2 shipped with macOS: no associative arrays, so maps keyed
# by ADR number or wire ID are indexed arrays, and `10#` keeps 0008 decimal.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

ALL_GATES=(
    frontmatter-present
    frontmatter-valid
    tldr-present
    dead-link
    adr-status
    adr-number-unique
    adr-index-sync
    adr-length
    adr-in-force-sync
    spec-version-sync
    spec-id-unique
    impl-status
)

# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

ONLY=""

usage() {
    cat <<'EOF'
check-docs.sh — mechanical enforcement for docs/CONVENTIONS.md

USAGE:
    bash scripts/check-docs.sh [--help] [--list] [--only=<gate>]

OPTIONS:
    --help          show this message and exit
    --list          print the gates this script implements and exit
    --only=<gate>   run only the named gate

Gates write violations to stderr prefixed with `[<gate-name>]`. The
script prints a `checked N files, M violations` summary on stdout and
exits 0 if M == 0, else 1.

See docs/CONVENTIONS.md for the contract these gates enforce.
EOF
}

list_gates() {
    echo "Gates implemented:"
    for g in "${ALL_GATES[@]}"; do
        echo "  - $g"
    done
}

for arg in "$@"; do
    case "$arg" in
        --help|-h) usage; exit 0 ;;
        --list) list_gates; exit 0 ;;
        --only=*) ONLY="${arg#--only=}" ;;
        *)
            echo "error: unknown argument: $arg" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ -n "$ONLY" && " ${ALL_GATES[*]} " != *" $ONLY "* ]]; then
    echo "error: --only=$ONLY does not match a known gate" >&2
    list_gates >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# File discovery
# ---------------------------------------------------------------------------

# Checked: top-level .md, docs/ (with docs/adr/), and research/ minus its
# archive. Exempt: the release-please CHANGELOG.md and third-party notices at
# the root (docs/spec/CHANGELOG.md is hand-maintained and keeps every gate),
# and docs/site/, the Astro site with its own conventions.
collect_files() {
    find "$ROOT" -maxdepth 1 -type f -name '*.md' \
        ! -iname 'LICENSE*' \
        ! -name 'CHANGELOG.md' \
        ! -name 'THIRD-PARTY-NOTICES.md' \
        -print
    find "$ROOT/docs" -type f -name '*.md' -not -path "$ROOT/docs/site/*" -print
    find "$ROOT/research" -type f -name '*.md' -not -path "$ROOT/research/archive/*" -print
}

FILES=()
while IFS= read -r f; do
    [[ -n "$f" ]] && FILES+=("$f")
done < <(collect_files | LC_ALL=C sort -u)

# The ADR corpus: docs/adr/NNNN-slug.md. Other files there (the index,
# IN-FORCE, companion documents) carry no decision, number or Status line.
adr_files() {
    find "$ROOT/docs/adr" -type f -name '[0-9][0-9][0-9][0-9]-*.md' | LC_ALL=C sort
}

# Echoes the ADR number of an adr_files path.
adr_number() {
    local base
    base="$(basename "$1")"
    printf '%s\n' "${base%%-*}"
}

# ---------------------------------------------------------------------------
# Violation bookkeeping
# ---------------------------------------------------------------------------

VIOLATIONS=0

violate() {
    # violate <gate> <file> <message...>
    local gate="$1"
    local file="$2"
    shift 2
    echo "[$gate] ${file#"$ROOT"/}: $*" >&2
    VIOLATIONS=$((VIOLATIONS + 1))
}

trim() {
    local s="$1"
    s="${s#"${s%%[![:space:]]*}"}"
    s="${s%"${s##*[![:space:]]}"}"
    printf '%s' "$s"
}

# ---------------------------------------------------------------------------
# Frontmatter parsing helpers
# ---------------------------------------------------------------------------

# Echoes the line number of the closing `---` if the file opens with a YAML
# block closed within the first 20 lines, else nothing.
frontmatter_close_line() {
    awk 'NR == 1 { if ($0 != "---") exit 0 }
         NR > 1 && NR <= 20 { if ($0 == "---") { print NR; exit 0 } }
         NR > 20 { exit 0 }' "$1"
}

# The root README declares the same metadata in an opening HTML comment
# instead, so the GitHub landing page shows no YAML. It is also exempt from
# the TL;DR gate.
is_root_readme() {
    [[ "$1" == "$ROOT/README.md" ]]
}

readme_html_close_line() {
    awk 'NR == 1 { if ($0 != "<!--") exit 0 }
         NR > 1 && NR <= 20 { if ($0 ~ /-->[[:space:]]*$/) { print NR; exit 0 } }
         NR > 20 { exit 0 }' "$1"
}

# First line after the frontmatter (1 when there is none).
content_start_line() {
    local close
    close="$(frontmatter_close_line "$1" || true)"
    echo $(( ${close:-0} + 1 ))
}

# Echoes the trimmed value for `key:` inside lines 2..close_line-1.
# (awk's `close` is a builtin, so the variable is named `close_line`.)
frontmatter_value() {
    local file="$1" key="$2" close_line="$3"
    awk -v key="$key" -v close_line="$close_line" '
        NR >= 2 && NR < close_line {
            if (match($0, "^[[:space:]]*" key ":[[:space:]]*")) {
                v = substr($0, RLENGTH + 1)
                gsub(/[[:space:]]+$/, "", v)
                print v
                exit
            }
        }
    ' "$file"
}

# ---------------------------------------------------------------------------
# frontmatter-present / frontmatter-valid / tldr-present
# ---------------------------------------------------------------------------

gate_frontmatter_present() {
    local file close key kind
    for file in "${FILES[@]}"; do
        if is_root_readme "$file"; then
            kind="HTML-comment metadata"
            close="$(readme_html_close_line "$file" || true)"
            if [[ -z "$close" ]]; then
                violate frontmatter-present "$file" \
                    "missing or malformed HTML-comment metadata (need '<!--' on line 1 and '-->' within the first 20 lines; README is exempt from YAML frontmatter — see docs/CONVENTIONS.md)"
                continue
            fi
        else
            kind="frontmatter"
            close="$(frontmatter_close_line "$file" || true)"
            if [[ -z "$close" ]]; then
                violate frontmatter-present "$file" \
                    "missing or malformed YAML frontmatter (need '---' on line 1 and a closing '---' within the first 20 lines)"
                continue
            fi
        fi
        local missing=()
        for key in audience stability last-reviewed; do
            [[ -n "$(frontmatter_value "$file" "$key" "$close" || true)" ]] || missing+=("$key")
        done
        if (( ${#missing[@]} > 0 )); then
            violate frontmatter-present "$file" "$kind missing key(s): ${missing[*]}"
        fi
    done
}

AUDIENCE_ALLOWED='^(humans|agents|consumers|contributors)$'
STABILITY_ALLOWED='^(stable|evolving|scratch)$'
DATE_RE='^[0-9]{4}-[0-9]{2}-[0-9]{2}$'

gate_frontmatter_valid() {
    local file close audience stability reviewed token bad
    for file in "${FILES[@]}"; do
        close="$(frontmatter_close_line "$file" || true)"
        # frontmatter-present already reported a missing header.
        [[ -z "$close" ]] && continue

        audience="$(frontmatter_value "$file" "audience" "$close" || true)"
        stability="$(frontmatter_value "$file" "stability" "$close" || true)"
        reviewed="$(frontmatter_value "$file" "last-reviewed" "$close" || true)"

        bad=""
        while IFS= read -r token; do
            token="$(trim "$token")"
            [[ -z "$token" || "$token" =~ $AUDIENCE_ALLOWED ]] || bad+="${bad:+, }$token"
        done < <(tr ',' '\n' <<< "$audience")
        if [[ -n "$bad" ]]; then
            violate frontmatter-valid "$file" \
                "audience: invalid value(s): $bad (allowed: humans, agents, consumers, contributors)"
        fi
        if [[ -n "$stability" && ! "$stability" =~ $STABILITY_ALLOWED ]]; then
            violate frontmatter-valid "$file" \
                "stability: invalid value '$stability' (allowed: stable, evolving, scratch)"
        fi
        if [[ -n "$reviewed" && ! "$reviewed" =~ $DATE_RE ]]; then
            violate frontmatter-valid "$file" \
                "last-reviewed: '$reviewed' is not a YYYY-MM-DD date"
        fi
    done
}

# The first non-blank line after the frontmatter and a single H1 must open
# with `**TL;DR.**`.
gate_tldr_present() {
    local file result
    for file in "${FILES[@]}"; do
        is_root_readme "$file" && continue
        result="$(awk -v start="$(content_start_line "$file")" '
            NR < start { next }
            {
                sub(/\r$/, "")
                if ($0 ~ /^[[:space:]]*$/) next
                if (!seen_h1 && $0 ~ /^#[[:space:]]/) { seen_h1 = 1; next }
                print ($0 ~ /^\*\*TL;DR\.\*\*/) ? "ok" : "bad:" $0
                printed = 1
                exit
            }
            END { if (!printed) print "empty" }
        ' "$file" || true)"
        case "$result" in
            ok) ;;
            bad:*)
                violate tldr-present "$file" \
                    "first content line is not a '**TL;DR.**' paragraph (saw: ${result#bad:})"
                ;;
            *)
                violate tldr-present "$file" \
                    "no content found after frontmatter (need an H1 and a '**TL;DR.**' paragraph)"
                ;;
        esac
    done
}

# ---------------------------------------------------------------------------
# dead-link
# ---------------------------------------------------------------------------

# Every `[text](target)` that is not a URL or pure anchor must resolve;
# `/x` resolves from the repo root. Parens inside targets are unsupported.
gate_dead_link() {
    local file dir link target resolved
    for file in "${FILES[@]}"; do
        dir="$(dirname "$file")"
        while IFS= read -r link; do
            case "$link" in
                ''|'#'*|http://*|https://*|mailto:*|ftp://*|tel:*|ws://*|wss://*) continue ;;
            esac
            target="${link%%#*}"
            target="${target%%\?*}"
            [[ -z "$target" ]] && continue
            case "$target" in
                /*) resolved="$ROOT$target" ;;
                *) resolved="$dir/$target" ;;
            esac
            [[ -e "$resolved" ]] || violate dead-link "$file" "broken relative link: $link"
        done < <(grep -oE '\]\([^)]+\)' "$file" 2>/dev/null \
                  | sed -E 's/^\]\(//; s/\)$//' \
                  | awk '{print $1}')
        # ^ awk {print $1} drops an optional `"title"`.
    done
}

# ---------------------------------------------------------------------------
# ADR gates
# ---------------------------------------------------------------------------

# Echoes the first `Status:` value outside the frontmatter, prefix stripped.
adr_status_value() {
    awk -v start="$(content_start_line "$1")" '
        NR < start { next }
        /^Status:/ {
            sub(/\r$/, "")
            sub(/^Status:[[:space:]]*/, "")
            sub(/[[:space:]]+$/, "")
            print
            found = 1
            exit
        }
        END { if (!found) print "<none>" }
    ' "$1" || true
}

gate_adr_status() {
    local file status
    while IFS= read -r file; do
        status="$(adr_status_value "$file")"
        case "$status" in
            "<none>")
                violate adr-status "$file" "no 'Status:' line found" ;;
            Proposed|Accepted|"Accepted (forward-compat)"|Deprecated) ;;
            "Superseded by ADR-"[0-9][0-9][0-9][0-9]) ;;
            *)
                violate adr-status "$file" \
                    "non-vocabulary status: 'Status: $status' (allowed: 'Status: Proposed', 'Status: Accepted', 'Status: Accepted (forward-compat)', 'Status: Superseded by ADR-NNNN', 'Status: Deprecated')"
                ;;
        esac
    done < <(adr_files)
}

gate_adr_number_unique() {
    local file num index
    local -a first_seen=()
    while IFS= read -r file; do
        num="$(adr_number "$file")"
        index=$((10#$num))
        if [[ -n "${first_seen[$index]:-}" ]]; then
            violate adr-number-unique "$file" \
                "ADR number $num is also used by ${first_seen[$index]#"$ROOT"/} — renumber one of the two (pick whichever has fewer inbound references) to the next free ADR number, then update every 'ADR-$num' cross-reference (prose, markdown links, code comments) that meant the renumbered file"
        else
            first_seen[$index]="$file"
        fi
    done < <(adr_files)
}

# ---------------------------------------------------------------------------
# Shared registry rows
# ---------------------------------------------------------------------------

# A shared registry is a tracked file with one row per hand-allocated
# identifier (the ADR index, the spec CHANGELOG, the wire-ID tables). Parallel
# branches can claim the same identifier without a git conflict; this is the
# backstop. It enforces (a) no key in two rows, (b) keys strictly ordered in
# `direction`, and (c) a linked row resolves to a file named `<key>-...`.
#
# Args: file label unit row_re rank_fn direction link_base hint
#   row_re    ERE; capture 1 = key, optional capture 2 = link target
#   rank_fn   maps a key to a distinct non-negative integer; non-zero exit
#             means "not a well-formed key"
#   link_base directory capture 2 resolves against ("" when linkless)
#
# Fills REGISTRY_ROW_TARGET (rank -> link target); a caller tests its size to
# detect a table whose shape no longer matches row_re.
REGISTRY_ROW_TARGET=()

check_registry_rows() {
    local file="$1" label="$2" unit="$3" row_re="$4" rank_fn="$5"
    local direction="$6" link_base="$7" hint="$8"

    REGISTRY_ROW_TARGET=()

    local line key target rank prev_key="" prev_rank=""
    while IFS= read -r line; do
        [[ "$line" =~ $row_re ]] || continue
        key="${BASH_REMATCH[1]}"
        target="${BASH_REMATCH[2]:-}"

        if [[ -n "$link_base" && -n "$target" ]]; then
            if [[ ! -f "$link_base/$target" ]]; then
                violate "$label" "$file" "row $key links to ./$target, which does not exist"
            elif [[ "$target" != "$key-"* ]]; then
                violate "$label" "$file" \
                    "row $key links to ./$target, whose filename does not start with $key-"
            fi
        fi

        if ! rank="$("$rank_fn" "$key")"; then
            violate "$label" "$file" \
                "row $unit '$key' is not a well-formed key for this registry"
            continue
        fi

        # The first row wins, so the reverse check compares against the row a
        # reader would follow.
        if [[ -n "${REGISTRY_ROW_TARGET[$rank]+set}" ]]; then
            violate "$label" "$file" \
                "more than one row for $unit $key — two branches allocated the same identifier; $hint"
        else
            REGISTRY_ROW_TARGET[$rank]="$target"
        fi

        if [[ -n "$prev_rank" ]] && registry_out_of_order "$direction" "$prev_rank" "$rank"; then
            violate "$label" "$file" \
                "row $key appears after row $prev_key — rows must run $direction; $hint"
        fi
        prev_key="$key"
        prev_rank="$rank"
    done < "$file"
}

# Fails when the last check_registry_rows call matched nothing, which means
# the table's shape changed and the registry check went inert.
require_registry_rows() {
    local label="$1" file="$2" what="$3"
    if (( ${#REGISTRY_ROW_TARGET[@]} == 0 )); then
        violate "$label" "$file" \
            "no $what rows matched - the table's shape changed and the registry check is now inert; fix the row pattern in scripts/check-docs.sh"
    fi
}

# Succeeds when `rank` does not strictly follow `prev_rank` in `direction`.
registry_out_of_order() {
    local direction="$1" prev_rank="$2" rank="$3"
    if [[ "$direction" == "ascending" ]]; then
        (( rank <= prev_rank ))
    else
        (( rank >= prev_rank ))
    fi
}

registry_rank_adr() {
    [[ "$1" =~ ^[0-9]{4}$ ]] || return 1
    printf '%d\n' "$((10#$1))"
}

# Rank for a docs/spec/CHANGELOG.md version, newest highest. The order is the
# file's, not semver's: within one version the `-draft.N` rows are wire
# changes made after the bare bump row, so they rank above it; the draft
# suffix compares numerically; an unnumbered `-draft` is the oldest draft.
# Rank = ((major*1000 + minor)*1000 + patch)*100000 + draft, where draft is 0
# for a bare release, 1 for `-draft`, N+2 for `-draft.N`.
registry_rank_spec_version() {
    local key="$1" core suffix draft
    core="${key%%-*}"
    [[ "$core" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]] || return 1
    local major="${BASH_REMATCH[1]}" minor="${BASH_REMATCH[2]}" patch="${BASH_REMATCH[3]}"

    suffix="${key#"$core"}"
    case "$suffix" in
        "")      draft=0 ;;
        -draft)  draft=1 ;;
        -draft.*)
            [[ "$suffix" =~ ^-draft\.([0-9]{1,4})$ ]] || return 1
            draft=$((10#${BASH_REMATCH[1]} + 2))
            ;;
        *)       return 1 ;;
    esac

    printf '%d\n' \
        "$(( ((10#$major * 1000 + 10#$minor) * 1000 + 10#$patch) * 100000 + draft ))"
}

# Rank for a wire-ID key such as `0x1A`: the byte value.
registry_rank_hex_byte() {
    [[ "$1" =~ ^0[xX][0-9A-Fa-f]{1,2}$ ]] || return 1
    local h="${1#0x}"
    h="${h#0X}"
    printf '%d\n' "$((16#$h))"
}

# ---------------------------------------------------------------------------
# adr-index-sync / adr-length / adr-in-force-sync
# ---------------------------------------------------------------------------

# Every ADR has exactly one ascending row in docs/adr/README.md linking to it.
gate_adr_index_sync() {
    local readme="$ROOT/docs/adr/README.md"
    if [[ ! -f "$readme" ]]; then
        violate adr-index-sync "$readme" "docs/adr/README.md not found — the ADR index is required"
        return
    fi

    check_registry_rows \
        "$readme" \
        adr-index-sync \
        "ADR number" \
        '^\|[[:space:]]*\[([0-9]{4})\]\(\./([^)]+)\)' \
        registry_rank_adr \
        ascending \
        "$ROOT/docs/adr" \
        "insert each new ADR's row at its numeric position, and renumber if a sibling branch took the number first"

    # Reverse direction: every ADR file has its row, pointing at this file.
    local file base num index
    while IFS= read -r file; do
        base="$(basename "$file")"
        num="$(adr_number "$file")"
        index=$((10#$num))
        if [[ -z "${REGISTRY_ROW_TARGET[$index]:-}" ]]; then
            violate adr-index-sync "$file" \
                "no index row in docs/adr/README.md for ADR number $num — every ADR adds exactly one row at its numeric position (see the comment above the index)"
        elif [[ "${REGISTRY_ROW_TARGET[$index]}" != "$base" ]]; then
            violate adr-index-sync "$file" \
                "index row $num links to ./${REGISTRY_ROW_TARGET[$index]}, not to this file — two files are claiming the same ADR number, or the row was not updated with a rename"
        fi
    done < <(adr_files)
}

# ADRs are capped at 150 lines. docs/adr/.length-baseline lists pre-gate
# violators (one NNNN per line, `#` comments allowed) and may only shrink: a
# listed ADR that fits, or an entry with no ADR, fails.
ADR_LENGTH_CAP=150

gate_adr_length() {
    local baseline="$ROOT/docs/adr/.length-baseline"
    local -a baselined=()
    local raw entry index
    if [[ -f "$baseline" ]]; then
        while IFS= read -r raw || [[ -n "$raw" ]]; do
            raw="${raw%$'\r'}"
            entry="$(trim "${raw%%#*}")"
            [[ -z "$entry" ]] && continue
            if ! [[ "$entry" =~ ^[0-9]{4}$ ]]; then
                violate adr-length "$baseline" "entry '$entry' is not a four-digit ADR number"
                continue
            fi
            baselined[$((10#$entry))]="$entry"
        done < "$baseline"
    fi

    local file num lines
    local -a seen=()
    while IFS= read -r file; do
        num="$(adr_number "$file")"
        index=$((10#$num))
        seen[$index]=1
        lines="$(awk 'END { print NR }' "$file")"
        if [[ -n "${baselined[$index]:-}" ]]; then
            if (( lines <= ADR_LENGTH_CAP )); then
                violate adr-length "$file" \
                    "$lines lines fits the $ADR_LENGTH_CAP-line cap; remove $num from docs/adr/.length-baseline in the same commit (entries may only be removed)"
            fi
        elif (( lines > ADR_LENGTH_CAP )); then
            violate adr-length "$file" \
                "$lines lines exceeds the $ADR_LENGTH_CAP-line cap (docs/CONVENTIONS.md, ADR template) — move the body to docs/architecture/ and have the ADR point at it; the baseline is closed to new entries"
        fi
    done < <(adr_files)

    if (( ${#baselined[@]} > 0 )); then
        for index in "${!baselined[@]}"; do
            if [[ -z "${seen[$index]:-}" ]]; then
                violate adr-length "$baseline" \
                    "entry ${baselined[$index]} names no docs/adr/${baselined[$index]}-*.md — remove it"
            fi
        done
    fi
}

# docs/adr/IN-FORCE.md links every Proposed/Accepted ADR exactly once and no
# Superseded/Deprecated one; every link resolves. A malformed Status line is
# adr-status's finding.
gate_adr_in_force_sync() {
    local view="$ROOT/docs/adr/IN-FORCE.md"
    if [[ ! -f "$view" ]]; then
        violate adr-in-force-sync "$view" \
            "docs/adr/IN-FORCE.md not found — the decisions-in-force view is required"
        return
    fi

    # One entry per grep -o match, so a line with two links counts both.
    local link num index target
    local -a link_count=()
    local -a link_target=()
    local link_re='^\[([0-9]{4})\]\(\./([^)#]+)'
    while IFS= read -r link; do
        [[ "$link" =~ $link_re ]] || continue
        num="${BASH_REMATCH[1]}"
        index=$((10#$num))
        target="${BASH_REMATCH[2]}"
        link_count[$index]=$(( ${link_count[$index]:-0} + 1 ))
        link_target[$index]="$target"

        if [[ ! -f "$ROOT/docs/adr/$target" ]]; then
            violate adr-in-force-sync "$view" "link $num points at ./$target, which does not exist"
        elif [[ "$target" != "$num-"* ]]; then
            violate adr-in-force-sync "$view" \
                "link $num points at ./$target, whose filename does not start with $num-"
        fi
    done < <(grep -oE '\[[0-9]{4}\]\(\./[^)]+\)' "$view" 2>/dev/null || true)

    local file base status count
    while IFS= read -r file; do
        base="$(basename "$file")"
        num="$(adr_number "$file")"
        index=$((10#$num))
        count="${link_count[$index]:-0}"
        status="$(adr_status_value "$file")"
        case "$status" in
            Proposed|Accepted|"Accepted (forward-compat)")
                if (( count == 0 )); then
                    violate adr-in-force-sync "$file" \
                        "no link in docs/adr/IN-FORCE.md for ADR $num (Status: $status) — add one line under the topic it governs, or under Proposed"
                elif (( count > 1 )); then
                    violate adr-in-force-sync "$view" \
                        "ADR $num is linked $count times — every in-force ADR appears exactly once"
                elif [[ "${link_target[$index]}" != "$base" ]]; then
                    violate adr-in-force-sync "$file" \
                        "docs/adr/IN-FORCE.md links $num to ./${link_target[$index]}, not to this file — two files are claiming the same ADR number, or the link was not updated with a rename"
                fi
                ;;
            "Superseded by ADR-"*|Deprecated)
                if (( count > 0 )); then
                    violate adr-in-force-sync "$view" \
                        "ADR $num is '$status' but still linked — remove its line; only Proposed and Accepted ADRs appear in the in-force view"
                fi
                ;;
        esac
    done < <(adr_files)
}

# ---------------------------------------------------------------------------
# spec-version-sync / spec-id-unique
# ---------------------------------------------------------------------------

# docs/spec/CHANGELOG.md's version rows form a descending registry, and its
# head version's MAJOR.MINOR.PATCH matches phux-protocol's PROTOCOL_VERSION.
gate_spec_version_sync() {
    local changelog="$ROOT/docs/spec/CHANGELOG.md"
    if [[ ! -f "$changelog" ]]; then
        violate spec-version-sync "$changelog" "docs/spec/CHANGELOG.md not found"
        return
    fi

    # Registry first, so a PROTOCOL_VERSION parse failure cannot mask it.
    check_registry_rows \
        "$changelog" \
        spec-version-sync \
        version \
        '^\|[[:space:]]*([0-9]+\.[0-9]+\.[0-9]+[^|[:space:]]*)[[:space:]]*\|' \
        registry_rank_spec_version \
        descending \
        "" \
        "the newest version goes at the top of the table; if a sibling branch already claimed this version, bump yours"
    require_registry_rows spec-version-sync "$changelog" version

    # Head version: the first table row's first cell, skipping header and
    # separator rows.
    local head_version
    head_version="$(awk '
        /^[[:space:]]*\|[[:space:]]*[^|[:space:]]+[[:space:]]*\|/ {
            line = $0
            sub(/^[[:space:]]*\|[[:space:]]*/, "", line)
            n = index(line, "|")
            if (n > 0) {
                v = substr(line, 1, n - 1)
                sub(/[[:space:]]+$/, "", v)
                if (v ~ /^[-: ]+$/) next
                if (tolower(v) == "version") next
                print v
                exit
            }
        }
    ' "$changelog")"

    if [[ -z "$head_version" ]]; then
        violate spec-version-sync "$changelog" "could not parse a head version from the first table row"
        return
    fi

    local lib="$ROOT/crates/phux-protocol/src/lib.rs"
    if [[ ! -f "$lib" ]]; then
        violate spec-version-sync "$lib" \
            "crates/phux-protocol/src/lib.rs not found; cannot verify PROTOCOL_VERSION"
        return
    fi

    local major minor patch
    major="$(awk '/PROTOCOL_VERSION/{flag=1} flag && /major:/{gsub(/[^0-9]/,"",$0); print; exit}' "$lib")"
    minor="$(awk '/PROTOCOL_VERSION/{flag=1} flag && /minor:/{gsub(/[^0-9]/,"",$0); print; exit}' "$lib")"
    patch="$(awk '/PROTOCOL_VERSION/{flag=1} flag && /patch:/{gsub(/[^0-9]/,"",$0); print; exit}' "$lib")"

    if [[ -z "$major" || -z "$minor" || -z "$patch" ]]; then
        violate spec-version-sync "$lib" "could not parse PROTOCOL_VERSION fields (major/minor/patch)"
        return
    fi

    local code_version="${major}.${minor}.${patch}"
    local changelog_core="${head_version%%-*}"
    if [[ "$changelog_core" != "$code_version" ]]; then
        violate spec-version-sync "$changelog" \
            "head version '$head_version' (core '$changelog_core') disagrees with PROTOCOL_VERSION '$code_version' in crates/phux-protocol/src/lib.rs"
    fi
}

# The spec's wire-ID tables are registries keyed on the ID column, each
# checked within its own file: the proto.md and L1.md message catalogs
# (five or more cells, which keeps L1.md's three-cell event-kind table out),
# L3.md's metadata frames (C->S and S->C rows each ascend), the backticked
# command-tag table in appendix-reserved.md, and L1.md's event kinds.
# check-spec-ids.awk then checks the shared cross-file message namespace;
# docs/spec/coordinator.md runs over its own connection and is checked alone.
spec_id_table() {
    local file="$1" unit="$2" row_re="$3" what="$4"
    [[ -f "$file" ]] || return 0
    check_registry_rows "$file" spec-id-unique "$unit" "$row_re" \
        registry_rank_hex_byte ascending "" \
        "allocate an open ID from docs/spec/appendix-reserved.md's reserved ranges and keep rows in ascending ID order; if a sibling branch already claimed this ID, renumber yours"
    require_registry_rows spec-id-unique "$file" "$what"
}

gate_spec_id_unique() {
    local spec="$ROOT/docs/spec"
    local msg_row='^\|[[:space:]]*(0x[0-9A-Fa-f]{2})[[:space:]]*(\|[^|]*){4}\|'
    spec_id_table "$spec/proto.md" "message ID" "$msg_row" message-ID
    spec_id_table "$spec/L1.md" "message ID" "$msg_row" message-ID
    spec_id_table "$spec/L3.md" "message ID" \
        '^\|[[:space:]]*(0x[0-9A-Fa-f]{2})[[:space:]]*\|[[:space:]]*C[[:space:]]' \
        "client-to-server message-ID"
    spec_id_table "$spec/L3.md" "message ID" \
        '^\|[[:space:]]*(0x[0-9A-Fa-f]{2})[[:space:]]*\|[[:space:]]*S[[:space:]]' \
        "server-to-client message-ID"
    spec_id_table "$spec/appendix-reserved.md" "command tag" \
        '^\|[[:space:]]*`(0x[0-9A-Fa-f]{2})`' command-tag
    spec_id_table "$spec/L1.md" "event kind" \
        '^\|[[:space:]]*(0x[0-9A-Fa-f]{2})[[:space:]]*\|[[:space:]]*`[a-z_]+`[[:space:]]*\|' \
        event-kind

    spec_id_violations \
        "$spec/appendix-reserved.md" \
        "$spec/input.md" \
        "$spec/L1.md" "$spec/L3.md" \
        "$spec/proto.md"
    spec_id_violations "$spec/coordinator.md"
}

spec_id_violations() {
    local violation
    while IFS= read -r violation; do
        violate spec-id-unique "$ROOT/docs/spec" "${violation//"$ROOT/"/}"
    done < <(awk -f "$ROOT/scripts/check-spec-ids.awk" "$@")
}

# ---------------------------------------------------------------------------
# impl-status
# ---------------------------------------------------------------------------

# Resolves every shipped/partial/spec-only claim (docs/CONVENTIONS.md
# §"Implementation status") against the code, in both directions. Carriers:
# catalog-table rows under docs/spec/ whose last cell is the status word
# (resolved against phux-protocol's wire constants), and prose markers
# `<!-- impl-status: S; probe: A,B -->` directly above a `> **Status` callout.

IMPL_STATUS_VOCAB='^(shipped|partial|spec-only|TBD)$'

# Crate sources with comment-only lines stripped, so a doc comment explaining
# that `RolePolicy` is unencoded does not count as its implementation. Built
# once, never inside a command substitution (a subshell would run the EXIT
# trap and delete the file).
IMPL_CODE_CACHE=""

impl_code_cache_init() {
    [[ -n "$IMPL_CODE_CACHE" ]] && return 0
    IMPL_CODE_CACHE="$(mktemp "${TMPDIR:-/tmp}/phux-impl-status.XXXXXX")"
    # shellcheck disable=SC2064
    trap "rm -f '$IMPL_CODE_CACHE'" EXIT
    find "$ROOT/crates" -type f -name '*.rs' -not -path '*/target/*' \
        -print0 \
        | xargs -0 cat \
        | grep -vE '^[[:space:]]*(//|/\*|\*)' > "$IMPL_CODE_CACHE" || true
}

# Does `probe` appear as a whole word on a non-comment crate line? Word
# boundaries are spelled out because `\b` is a GNU extension.
probe_matches() {
    grep -qE "(^|[^A-Za-z0-9_])${1}([^A-Za-z0-9_]|\$)" "$IMPL_CODE_CACHE"
}

# Is there a TYPE_/COMMAND_TAG_ discriminant for this name? The trailing colon
# keeps `TYPE_SUBSCRIBE` from matching `TYPE_SUBSCRIBE_EVENTS`.
WIRE_CONSTS=""

wire_const_exists() {
    if [[ -z "$WIRE_CONSTS" ]]; then
        WIRE_CONSTS="$(grep -rhoE 'const (TYPE|COMMAND_TAG)_[A-Z0-9_]+:' \
            "$ROOT/crates/phux-protocol/src/wire" 2>/dev/null || true)"
    fi
    printf '%s\n' "$WIRE_CONSTS" | grep -qxE "const (TYPE|COMMAND_TAG)_${1}:"
}

impl_status_tables() {
    local file line row last name fenced
    local name_re='`([A-Z][A-Z0-9_]+)`'
    for file in "${FILES[@]}"; do
        [[ "$file" == "$ROOT"/docs/spec/* ]] || continue
        # The changelog narrates past releases, not the current tree.
        [[ "$file" == "$ROOT/docs/spec/CHANGELOG.md" ]] && continue

        fenced=0
        while IFS= read -r line; do
            if [[ "$line" == '```'* ]]; then
                fenced=$((1 - fenced))
                continue
            fi
            [[ "$fenced" -eq 1 ]] && continue
            [[ "$line" == \|* ]] || continue
            row="$(trim "$line")"
            row="${row%|}"
            last="$(trim "${row##*|}")"
            [[ "$last" =~ $IMPL_STATUS_VOCAB ]] || continue

            if ! [[ "$row" =~ $name_re ]]; then
                violate impl-status "$file" \
                    "table row claims '$last' but names no \`MESSAGE_NAME\` to resolve it against: $row"
                continue
            fi
            name="${BASH_REMATCH[1]}"

            if wire_const_exists "$name"; then
                if [[ "$last" == spec-only || "$last" == TBD ]]; then
                    violate impl-status "$file" \
                        "\`$name\` is marked '$last' but crates/phux-protocol/src/wire/ declares its discriminant — the codec shipped and the doc did not follow"
                fi
            elif [[ "$last" == shipped || "$last" == partial ]]; then
                violate impl-status "$file" \
                    "\`$name\` is marked '$last' but no TYPE_$name / COMMAND_TAG_$name constant exists in crates/phux-protocol/src/wire/ — mark it 'spec-only' or wire it up"
            fi
        done < "$file"
    done
}

# Checks one marker's probes against the code.
impl_status_check_probes() {
    local file="$1" lineno="$2" status="$3" probes="$4" probe
    local probe_re='^[A-Za-z_][A-Za-z0-9_:-]*$'
    local IFS=','
    for probe in $probes; do
        if ! [[ "$probe" =~ $probe_re ]]; then
            violate impl-status "$file" \
                "line $lineno: probe '$probe' is not a plain symbol (letters, digits, '_', ':', '-')"
        elif probe_matches "$probe"; then
            if [[ "$status" == "spec-only" || "$status" == "TBD" ]]; then
                violate impl-status "$file" \
                    "line $lineno: marked '$status' but probe '$probe' matches non-comment code under crates/ — the implementation landed and the prose did not follow"
            fi
        elif [[ "$status" == "shipped" || "$status" == "partial" ]]; then
            violate impl-status "$file" \
                "line $lineno: marked '$status' but probe '$probe' matches no non-comment code under crates/ — fix the probe or the claim"
        fi
    done
}

# Every marker needs a `> **Status` callout after it; under docs/spec/ and
# docs/consumers/ every callout needs a marker before it. Fenced blocks are
# examples, not claims.
impl_status_markers() {
    local file line lineno status probes scoped pending_marker fenced
    local marker_re='^<!--[[:space:]]*impl-status:[[:space:]]*([A-Za-z-]+);[[:space:]]*probe:[[:space:]]*([A-Za-z0-9_,:-]+)[[:space:]]*-->[[:space:]]*$'
    for file in "${FILES[@]}"; do
        scoped=0
        case "$file" in
            "$ROOT"/docs/spec/*|"$ROOT"/docs/consumers/*) scoped=1 ;;
        esac

        lineno=0
        fenced=0
        pending_marker=""
        while IFS= read -r line; do
            lineno=$((lineno + 1))
            line="${line%$'\r'}"

            if [[ "$line" == '```'* ]]; then
                fenced=$((1 - fenced))
                continue
            fi
            [[ "$fenced" -eq 1 ]] && continue

            if [[ "$line" =~ $marker_re ]]; then
                status="${BASH_REMATCH[1]}"
                probes="${BASH_REMATCH[2]}"
                if [[ "$status" =~ $IMPL_STATUS_VOCAB ]]; then
                    impl_status_check_probes "$file" "$lineno" "$status" "$probes"
                    pending_marker="$status"
                else
                    violate impl-status "$file" \
                        "line $lineno: unknown impl-status '$status' (allowed: shipped, partial, spec-only, TBD)"
                    pending_marker="invalid"
                fi
                continue
            fi

            # Blank lines neither satisfy nor clear a pending marker.
            [[ "$line" =~ ^[[:space:]]*$ ]] && continue

            if [[ "$line" == '> **Status'* ]]; then
                if [[ -z "$pending_marker" && "$scoped" -eq 1 ]]; then
                    violate impl-status "$file" \
                        "line $lineno: '> **Status' callout with no '<!-- impl-status: ... -->' marker above it (docs/CONVENTIONS.md §Implementation status)"
                fi
            elif [[ -n "$pending_marker" ]]; then
                violate impl-status "$file" \
                    "line $lineno: impl-status marker is not followed by a '> **Status' callout; the machine-readable claim needs a reader-visible one"
            fi
            pending_marker=""
        done < "$file"

        if [[ -n "$pending_marker" ]]; then
            violate impl-status "$file" \
                "trailing impl-status marker with no '> **Status' callout after it"
        fi
    done
}

gate_impl_status() {
    impl_code_cache_init
    impl_status_tables
    impl_status_markers
}

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

for gate in "${ALL_GATES[@]}"; do
    [[ -z "$ONLY" || "$ONLY" == "$gate" ]] || continue
    "gate_${gate//-/_}"
done

echo "checked ${#FILES[@]} files, ${VIOLATIONS} violations"

(( VIOLATIONS == 0 ))
