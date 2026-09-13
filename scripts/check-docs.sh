#!/usr/bin/env bash
# check-docs.sh
#
# Doc-system gates for the phux repository. Enforces the contract laid
# out in docs/CONVENTIONS.md (the "discipline" layer of the doc tree).
#
# Gates:
#   - frontmatter-present : every checked .md has a YAML header with
#                           audience/stability/last-reviewed
#   - frontmatter-valid   : that header's values match the controlled
#                           vocabulary
#   - tldr-present        : first non-frontmatter / non-H1 paragraph
#                           begins with "**TL;DR.**"
#   - dead-link           : every relative `[text](path.md...)` link
#                           resolves to a real file
#   - adr-status          : every ADR's `Status:` line is one of the
#                           four blessed forms
#   - adr-number-unique   : no two files under ADR/ share the same
#                           leading NNNN number
#   - adr-index-sync      : every ADR/NNNN-*.md has exactly one row in
#                           ADR/README.md's index, every row resolves to
#                           its file, and rows ascend numerically
#   - adr-length          : every ADR/NNNN-*.md is at most 150 lines
#                           unless listed in ADR/.length-baseline, and a
#                           listed ADR that fits the cap is removed from
#                           the baseline (entries may only be removed)
#   - adr-in-force-sync   : every Proposed/Accepted ADR is linked exactly
#                           once in ADR/IN-FORCE.md, no Superseded or
#                           Deprecated ADR is linked, and every link
#                           resolves to its file
#   - spec-version-sync   : docs/spec/CHANGELOG.md head version agrees
#                           with phux-protocol's PROTOCOL_VERSION, and its
#                           version rows are unique and strictly descending
#                           (skipped while the SPEC split is in flight)
#   - spec-id-unique      : every wire-ID row in the spec allocation tables
#                           (message IDs, command tags, agent event kinds)
#                           is unique and strictly ascending, so two
#                           branches cannot claim the same wire ID
#   - impl-status         : every shipped/partial/spec-only claim in
#                           docs/spec/ and docs/consumers/ agrees with
#                           the code it names
#
# Usage:
#   bash scripts/check-docs.sh             # run all gates
#   bash scripts/check-docs.sh --help
#   bash scripts/check-docs.sh --list
#   bash scripts/check-docs.sh --only=tldr-present
#
# Exit codes:
#   0   no violations
#   1   one or more violations
#   2   script invoked incorrectly

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
        --help|-h)
            usage
            exit 0
            ;;
        --list)
            list_gates
            exit 0
            ;;
        --only=*)
            ONLY="${arg#--only=}"
            ;;
        *)
            echo "error: unknown argument: $arg" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ -n "$ONLY" ]]; then
    found=0
    for g in "${ALL_GATES[@]}"; do
        if [[ "$g" == "$ONLY" ]]; then
            found=1
            break
        fi
    done
    if [[ "$found" -eq 0 ]]; then
        echo "error: --only=$ONLY does not match a known gate" >&2
        list_gates >&2
        exit 2
    fi
fi

should_run() {
    local gate="$1"
    [[ -z "$ONLY" || "$ONLY" == "$gate" ]]
}

# ---------------------------------------------------------------------------
# File discovery
# ---------------------------------------------------------------------------

# Build the list of .md files we care about. Exclusions, per CONVENTIONS.md:
#   - LICENSE* (not markdown anyway, but defensive)
#   - .beads/, .direnv/, .git/, target/, research/archive/
#   - crates/*/tests/*/README.md (test fixtures, not part of the doc system)
#   - docs/site/ (the imported Astro site app — it carries its own doc
#     conventions, see docs/site/FUMADOCS.md and docs/site/AGENTS.md; the
#     doc-system contract below governs the hand-maintained docs tree, not
#     site content or site infra notes)
# Inclusions:
#   - everything under docs/ and ADR/
#   - top-level .md (README, AGENTS, CLAUDE, CONTRIBUTING, ARCHITECTURE,
#     SPEC, DESIGN, VISION)
#   - research/ (excluding research/archive/) — `stability: scratch`
#     lives here and CONVENTIONS.md says frontmatter is still required.

collect_files() {
    # Top-level .md files.
    #
    # CHANGELOG.md is exempt: it is generated by release-please from the
    # conventional-commit log and rewritten on every release. It has no
    # author to carry frontmatter, and a `**TL;DR.**` block would be
    # clobbered on the next release anyway. Note this exclusion is scoped
    # to the repo root — docs/spec/CHANGELOG.md (the normative wire log,
    # hand-maintained) is collected by the docs/ sweep below and keeps
    # every gate.
    find "$ROOT" -maxdepth 1 -type f -name '*.md' \
        ! -iname 'LICENSE*' \
        ! -name 'CHANGELOG.md' \
        -print
    # docs/ tree.
    if [[ -d "$ROOT/docs" ]]; then
        find "$ROOT/docs" -type f -name '*.md' \
            -not -path "$ROOT/docs/site/*" \
            -print
    fi
    # ADR/ tree.
    if [[ -d "$ROOT/ADR" ]]; then
        find "$ROOT/ADR" -type f -name '*.md' -print
    fi
    # research/, minus archive/.
    if [[ -d "$ROOT/research" ]]; then
        find "$ROOT/research" -type f -name '*.md' \
            -not -path "$ROOT/research/archive/*" \
            -print
    fi
}

FILES=()
while IFS= read -r f; do
    [[ -n "$f" ]] && FILES+=("$f")
done < <(collect_files | LC_ALL=C sort -u)

# The ADR corpus proper, sorted: every file under ADR/ whose basename opens
# with a four-digit number (`NNNN-slug.md`).
#
# One definition, because three gates need it and each used to carry its own
# basename test — so "what counts as an ADR file" had three answers that only
# happened to agree. Everything else under ADR/ (the README index, companion
# documents such as a ratification brief) carries no decision and no ADR
# number: it must not be given a `Status:` line, cannot claim a number, and
# has no index row. Those files still keep every gate in `collect_files`,
# which sweeps ADR/ whole.
adr_files() {
    if [[ ! -d "$ROOT/ADR" ]]; then
        return
    fi
    find "$ROOT/ADR" -type f -name '[0-9][0-9][0-9][0-9]-*.md' | LC_ALL=C sort
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
    local rel="${file#$ROOT/}"
    echo "[$gate] $rel: $*" >&2
    VIOLATIONS=$((VIOLATIONS + 1))
}

# ---------------------------------------------------------------------------
# Frontmatter parsing helpers
# ---------------------------------------------------------------------------

# Echoes the line number (1-based) of the closing `---` if the file opens
# with a YAML frontmatter block within the first 20 lines, else nothing.
frontmatter_close_line() {
    local file="$1"
    awk 'NR == 1 { if ($0 != "---") exit 0 }
         NR > 1 && NR <= 20 { if ($0 == "---") { print NR; exit 0 } }
         NR > 20 { exit 0 }' "$file"
}

# Echoes the value (everything after `key:`) for a frontmatter key, trimmed.
# Only looks inside the frontmatter block (lines 2..close-1).
# (awk's `close` is a builtin, so the variable is named `close_line`.)
frontmatter_value() {
    local file="$1"
    local key="$2"
    local close_line="$3"
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

# Is this the repo-root README.md? It is exempt from YAML frontmatter
# (would render visibly on GitHub as the project landing page) and
# from the TL;DR gate (the README's whole job is to be the landing
# page, not to summarize itself). Instead, it declares the same
# metadata via an opening HTML comment block within the first 20
# lines. See docs/CONVENTIONS.md.
is_root_readme() {
    [[ "$1" == "$ROOT/README.md" ]]
}

# For the README only: echoes the closing-comment line if an HTML
# comment block opens on line 1 and closes within the first 20 lines.
readme_html_close_line() {
    local file="$1"
    awk 'NR == 1 { if ($0 != "<!--") exit 0 }
         NR > 1 && NR <= 20 { if ($0 ~ /-->[[:space:]]*$/) { print NR; exit 0 } }
         NR > 20 { exit 0 }' "$file"
}

# ---------------------------------------------------------------------------
# Gate 1: frontmatter-present
# ---------------------------------------------------------------------------

gate_frontmatter_present() {
    local file close
    for file in "${FILES[@]}"; do
        # README.md exception: HTML-comment metadata in place of YAML.
        if is_root_readme "$file"; then
            close="$(readme_html_close_line "$file" || true)"
            if [[ -z "$close" ]]; then
                violate frontmatter-present "$file" \
                    "missing or malformed HTML-comment metadata (need '<!--' on line 1 and '-->' within the first 20 lines; README is exempt from YAML frontmatter — see docs/CONVENTIONS.md)"
                continue
            fi
            local rmissing=()
            for key in audience stability last-reviewed; do
                if ! awk -v key="$key" -v close_line="$close" '
                        NR >= 2 && NR < close_line {
                            if (match($0, "^[[:space:]]*" key ":[[:space:]]*[^[:space:]]")) { found = 1; exit }
                        }
                        END { exit (found ? 0 : 1) }
                    ' "$file"; then
                    rmissing+=("$key")
                fi
            done
            if (( ${#rmissing[@]} > 0 )); then
                violate frontmatter-present "$file" \
                    "HTML-comment metadata missing key(s): ${rmissing[*]}"
            fi
            continue
        fi

        close="$(frontmatter_close_line "$file" || true)"
        if [[ -z "$close" ]]; then
            violate frontmatter-present "$file" \
                "missing or malformed YAML frontmatter (need '---' on line 1 and a closing '---' within the first 20 lines)"
            continue
        fi
        local missing=()
        for key in audience stability last-reviewed; do
            local val
            val="$(frontmatter_value "$file" "$key" "$close" || true)"
            if [[ -z "$val" ]]; then
                missing+=("$key")
            fi
        done
        if (( ${#missing[@]} > 0 )); then
            violate frontmatter-present "$file" \
                "frontmatter missing key(s): ${missing[*]}"
        fi
    done
}

# ---------------------------------------------------------------------------
# Gate 2: frontmatter-valid
# ---------------------------------------------------------------------------

# Allowed sets.
AUDIENCE_ALLOWED='^(humans|agents|consumers|contributors)$'
STABILITY_ALLOWED='^(stable|evolving|scratch)$'
DATE_RE='^[0-9]{4}-[0-9]{2}-[0-9]{2}$'

gate_frontmatter_valid() {
    local file close
    for file in "${FILES[@]}"; do
        close="$(frontmatter_close_line "$file" || true)"
        # If there's no frontmatter, gate 1 already complained; skip here.
        [[ -z "$close" ]] && continue

        local audience stability reviewed
        audience="$(frontmatter_value "$file" "audience" "$close" || true)"
        stability="$(frontmatter_value "$file" "stability" "$close" || true)"
        reviewed="$(frontmatter_value "$file" "last-reviewed" "$close" || true)"

        # audience: comma-separated list of allowed words.
        if [[ -n "$audience" ]]; then
            local cleaned
            cleaned="$(echo "$audience" | tr ',' '\n' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
            local bad=""
            while IFS= read -r token; do
                [[ -z "$token" ]] && continue
                if ! [[ "$token" =~ $AUDIENCE_ALLOWED ]]; then
                    bad+="${bad:+, }$token"
                fi
            done <<< "$cleaned"
            if [[ -n "$bad" ]]; then
                violate frontmatter-valid "$file" \
                    "audience: invalid value(s): $bad (allowed: humans, agents, consumers, contributors)"
            fi
        fi

        # stability: single word from the allowed set.
        if [[ -n "$stability" ]]; then
            if ! [[ "$stability" =~ $STABILITY_ALLOWED ]]; then
                violate frontmatter-valid "$file" \
                    "stability: invalid value '$stability' (allowed: stable, evolving, scratch)"
            fi
        fi

        # last-reviewed: ISO date.
        if [[ -n "$reviewed" ]]; then
            if ! [[ "$reviewed" =~ $DATE_RE ]]; then
                violate frontmatter-valid "$file" \
                    "last-reviewed: '$reviewed' is not a YYYY-MM-DD date"
            fi
        fi
    done
}

# ---------------------------------------------------------------------------
# Gate 3: tldr-present
# ---------------------------------------------------------------------------

# Per CONVENTIONS.md: the first non-blank, non-frontmatter, non-H1 line
# of content must start with `**TL;DR.**`. We tolerate any number of
# blank lines between the H1 and the TL;DR, and we treat the absence of
# a frontmatter block as "skip" — gate 1 already flagged that case.

gate_tldr_present() {
    local file close
    for file in "${FILES[@]}"; do
        # README.md is exempt — its whole job is to be the landing page;
        # a TL;DR paragraph would degrade that. See docs/CONVENTIONS.md.
        if is_root_readme "$file"; then
            continue
        fi
        close="$(frontmatter_close_line "$file" || true)"
        local start=1
        if [[ -n "$close" ]]; then
            start=$((close + 1))
        fi
        local result
        result="$(awk -v start="$start" '
            NR < start { next }
            {
                # strip CR
                sub(/\r$/, "")
                # skip blank lines
                if ($0 ~ /^[[:space:]]*$/) next
                # skip a single H1 (first non-blank H1 only)
                if (!seen_h1 && $0 ~ /^#[[:space:]]/) {
                    seen_h1 = 1
                    next
                }
                # first non-blank, non-H1 line of content.
                if ($0 ~ /^\*\*TL;DR\.\*\*/) {
                    print "ok"
                } else {
                    print "bad:" $0
                }
                printed = 1
                exit
            }
            END {
                if (!printed) print "empty"
            }
        ' "$file" || true)"
        case "$result" in
            ok) ;;
            bad:*)
                violate tldr-present "$file" \
                    "first content line is not a '**TL;DR.**' paragraph (saw: ${result#bad:})"
                ;;
            empty|"")
                violate tldr-present "$file" \
                    "no content found after frontmatter (need an H1 and a '**TL;DR.**' paragraph)"
                ;;
        esac
    done
}

# ---------------------------------------------------------------------------
# Gate 4: dead-link
# ---------------------------------------------------------------------------

# Match `[text](path)` where `path` does not start with a scheme
# (http://, https://, mailto:, #...) and is not absolute (`/...`).
# Anchors (`#...`) on the end are stripped before resolving.

gate_dead_link() {
    local file
    for file in "${FILES[@]}"; do
        local dir
        dir="$(dirname "$file")"
        # Extract every `](...)` target. We use grep -oE then awk-clean.
        # The regex purposely keeps it simple: balanced parens inside the
        # URL aren't supported (markdown's spec discourages them anyway).
        while IFS= read -r link; do
            [[ -z "$link" ]] && continue
            # Skip absolute URLs.
            case "$link" in
                http://*|https://*|mailto:*|ftp://*|tel:*|ws://*|wss://*)
                    continue ;;
                '#'*)
                    # Pure in-page anchor — nothing to resolve.
                    continue ;;
                /*)
                    # Repo-absolute or filesystem-absolute. Resolve from
                    # repo root rather than treating it as filesystem-/.
                    : ;;
            esac

            # Strip anchor and optional query.
            local target="${link%%#*}"
            target="${target%%\?*}"
            [[ -z "$target" ]] && continue

            local resolved
            case "$target" in
                /*)
                    resolved="$ROOT$target"
                    ;;
                *)
                    resolved="$dir/$target"
                    ;;
            esac

            if [[ ! -e "$resolved" ]]; then
                violate dead-link "$file" \
                    "broken relative link: $link"
            fi
        done < <(grep -oE '\]\([^)]+\)' "$file" 2>/dev/null \
                  | sed -E 's/^\]\(//; s/\)$//' \
                  | awk '{print $1}')
        # ^ awk {print $1} strips any optional title `](path "title")`.
    done
}

# ---------------------------------------------------------------------------
# Gate 5: adr-status
# ---------------------------------------------------------------------------

# Allowed statuses (anchored, exact):
#   Status: Proposed
#   Status: Accepted
#   Status: Accepted (forward-compat)
#   Status: Superseded by ADR-NNNN
#   Status: Deprecated

gate_adr_status() {
    local file
    if [[ ! -d "$ROOT/ADR" ]]; then
        return
    fi
    # `adr_files` is what "an ADR" means here: the README index and companion
    # documents carry no decision and must not be given a `Status:` line,
    # since that vocabulary means "the repository has decided".
    while IFS= read -r file; do
        local close
        close="$(frontmatter_close_line "$file" || true)"
        local start=1
        [[ -n "$close" ]] && start=$((close + 1))

        # First `Status:` line outside frontmatter.
        local status_line
        status_line="$(awk -v start="$start" '
            NR < start { next }
            /^Status:/ { sub(/\r$/, ""); print; exit }
        ' "$file" || true)"

        if [[ -z "$status_line" ]]; then
            violate adr-status "$file" "no 'Status:' line found"
            continue
        fi

        case "$status_line" in
            "Status: Proposed"|\
            "Status: Accepted"|\
            "Status: Accepted (forward-compat)"|\
            "Status: Deprecated")
                ;;
            "Status: Superseded by ADR-"[0-9][0-9][0-9][0-9])
                ;;
            *)
                violate adr-status "$file" \
                    "non-vocabulary status: '$status_line' (allowed: 'Status: Proposed', 'Status: Accepted', 'Status: Accepted (forward-compat)', 'Status: Superseded by ADR-NNNN', 'Status: Deprecated')"
                ;;
        esac
    done < <(adr_files)
}

# ---------------------------------------------------------------------------
# Gate 5b: adr-number-unique
# ---------------------------------------------------------------------------

# Every ADR filename opens with a 4-digit number (NNNN-slug.md). That number
# is the identity readers and cross-references use ("ADR-NNNN") — two files
# sharing one make every such reference ambiguous, with no signal to the
# reader that they might have landed on the wrong decision. This gate fails
# if any number prefix under ADR/ is claimed by more than one file.

gate_adr_number_unique() {
    local file
    if [[ ! -d "$ROOT/ADR" ]]; then
        return
    fi

    # ADR IDs are bounded decimal integers; indexed arrays also work in the
    # Bash 3.2 shipped on macOS. Convert explicitly so 0008 is not octal.
    local -a first_seen=()
    while IFS= read -r file; do
        # `adr_files` yields only `NNNN-slug.md`; this reads the number out of
        # the name rather than deciding again what counts as an ADR.
        [[ "$(basename "$file")" =~ ^([0-9]{4})- ]] || continue
        local num="${BASH_REMATCH[1]}"
        local index=$((10#$num))
        if [[ -n "${first_seen[$index]:-}" ]]; then
            violate adr-number-unique "$file" \
                "ADR number $num is also used by ${first_seen[$index]#"$ROOT"/} — renumber one of the two (pick whichever has fewer inbound references) to the next free ADR number, then update every 'ADR-$num' cross-reference (prose, markdown links, code comments) that meant the renumbered file"
        else
            first_seen[$index]="$file"
        fi
    done < <(adr_files)
}

# ---------------------------------------------------------------------------
# Shared: registry rows
# ---------------------------------------------------------------------------

# A *shared registry* is a tracked file carrying one row per hand-allocated
# identifier: ADR/README.md's index (one row per ADR number) and
# docs/spec/CHANGELOG.md (one row per wire version). Parallel branches collide
# in them without git noticing — each branch reads the same "next free"
# identifier, claims it, and if the rows land in different places the merge is
# clean and the collision is silent. It has happened here: two different
# ADR-0086 files in wave 3, and — one batch after the ADR index gate landed —
# two branches both claiming spec CHANGELOG row 0.8.0-draft.4, which the
# head-row-only version check passed.
#
# The primary defense is still textual: a registry row is an anchor every
# claimant edits, so two claims usually conflict at rebase. This helper is the
# backstop for when they do not, and the overlap with the per-file gates
# (adr-number-unique) is deliberate — see docs/CONVENTIONS.md §"The index row".
#
# What it enforces on any registry:
#
#   (a) no key appears in two rows
#   (b) keys run strictly in the declared direction down the file
#   (c) where a row carries a link, the link resolves to a real file whose
#       name starts with the key
#
# Arguments:
#   $1 file       registry file to read
#   $2 label      gate label passed to `violate`
#   $3 unit       what a key is, for messages ("ADR number", "version")
#   $4 row_re     ERE matched per line; capture 1 = key, capture 2 = link
#                 target (omit the second group for a linkless registry)
#   $5 rank_fn    function mapping a key to a non-negative integer on stdout,
#                 distinct keys to distinct integers; non-zero exit means
#                 "not a well-formed key"
#   $6 direction  `ascending` or `descending`
#   $7 link_base  directory capture 2 resolves against, "" when linkless
#   $8 hint       remediation sentence appended to duplicate/order messages
#
# Fills REGISTRY_ROW_TARGET (rank -> link target, "" when linkless) for callers
# that need the reverse direction or a row count; cleared on entry. It is an
# indexed array keyed by rank, not an associative one, because the script runs
# on the Bash 3.2 shipped with macOS. Callers wanting a "the table went
# missing" check test its size afterwards — this helper does not, because an
# empty registry is legitimate for some (an ADR-less tree).

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

        registry_check_link "$file" "$label" "$key" "$target" "$link_base"

        if ! rank="$("$rank_fn" "$key")"; then
            violate "$label" "$file" \
                "row $unit '$key' is not a well-formed key for this registry"
            continue
        fi

        # A rank already in the map *is* the second-row signal: two parallel
        # counters said the same thing twice. The first row wins, so a caller
        # checking the reverse direction compares against the row a reader
        # would actually follow.
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

# Check (c) for one row: a linked row must resolve under $link_base to a file
# whose name starts with the row's key. No-op for a linkless registry or row.
registry_check_link() {
    local file="$1" label="$2" key="$3" target="$4" link_base="$5"
    [[ -n "$link_base" && -n "$target" ]] || return 0
    if [[ ! -f "$link_base/$target" ]]; then
        violate "$label" "$file" \
            "row $key links to ./$target, which does not exist"
    elif [[ "$target" != "$key-"* ]]; then
        violate "$label" "$file" \
            "row $key links to ./$target, whose filename does not start with $key-"
    fi
}

# Check (b) for one adjacent pair: succeeds when `rank` does not strictly
# follow `prev_rank` in `direction` (equal ranks are out of order too).
registry_out_of_order() {
    local direction="$1" prev_rank="$2" rank="$3"
    if [[ "$direction" == "ascending" ]]; then
        (( rank <= prev_rank ))
    else
        (( rank >= prev_rank ))
    fi
}

# Rank for an ADR index key: the number itself. `10#` forces decimal, so a
# leading zero (0086) is not read as octal.
registry_rank_adr() {
    [[ "$1" =~ ^[0-9]{4}$ ]] || return 1
    printf '%d\n' "$((10#$1))"
}

# Rank for a docs/spec/CHANGELOG.md version key.
#
# The ordering rule is DERIVED FROM THE FILE, not from semver precedence,
# which disagrees with it. Reading the table top to bottom:
#
#   0.9.0-draft.1, 0.9.0, 0.8.0-draft.17 … 0.8.0-draft.1, 0.8.0,
#   0.7.0-draft.11 … 0.7.0, …, 0.2.0-draft.1, 0.2.0-draft,
#   0.1.0-draft.7 … 0.1.0-draft
#
# so, newest first:
#
#   1. compare major, then minor, then patch, numerically;
#   2. within one version the DRAFTS SIT ABOVE THE BARE ROW. The bare row
#      records the version bump itself; each `-draft.N` after it is a wire
#      change made under that version and therefore newer. (Semver says the
#      opposite — 0.8.0 > 0.8.0-draft.6 — which is why this comparator is
#      hand-written rather than delegated to a version sort.)
#   3. the draft suffix is a NUMBER: draft.11 is newer than draft.9, not
#      older as a string sort would have it.
#   4. an unnumbered `-draft` (0.2.0-draft, 0.1.0-draft, from before the
#      suffix was numbered) is the oldest draft of its version, still above
#      a bare row for that version.
#
# Rank = ((major*1000 + minor)*1000 + patch) * 100000 + draft_rank, with
# draft_rank 0 for a bare release, 1 for `-draft`, N+2 for `-draft.N` (so
# `-draft` and `-draft.0` stay distinct keys).
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

# ---------------------------------------------------------------------------
# Gate 5c: adr-index-sync
# ---------------------------------------------------------------------------

# Every ADR has exactly one row in ADR/README.md's index, inserted at its
# numeric position. The row is deliberately a collision point for parallel
# branches: two branches that each claim the same ADR number either conflict
# textually on the index row at rebase, or — if the merge somehow slides
# through — fail here. Without the row, git sees two disjoint new files and
# reports zero conflicts (it happened: wave 3 produced two different
# ADR-0086 files, silently). Three checks, bidirectional:
#
#   1. every ADR/NNNN-*.md file has exactly one index row for its number
#   2. every index row's link resolves to a real file whose name starts
#      with the row's number
#   3. row numbers are strictly ascending down the table

gate_adr_index_sync() {
    if [[ ! -d "$ROOT/ADR" ]]; then
        return
    fi
    local readme="$ROOT/ADR/README.md"
    if [[ ! -f "$readme" ]]; then
        violate adr-index-sync "$readme" \
            "ADR/README.md not found — the ADR index is required"
        return
    fi

    # Checks 2-3, plus no number claimed by two rows, are the shared registry
    # contract over index rows `| [NNNN](./NNNN-slug.md) | ... |`: ascending,
    # link-resolving.
    check_registry_rows \
        "$readme" \
        adr-index-sync \
        "ADR number" \
        '^\|[[:space:]]*\[([0-9]{4})\]\(\./([^)]+)\)' \
        registry_rank_adr \
        ascending \
        "$ROOT/ADR" \
        "insert each new ADR's row at its numeric position, and renumber if a sibling branch took the number first"

    # Check 1, the reverse direction, is specific to this registry because
    # only ADRs have a file per row: every ADR file has its row, and the row
    # points at this file (not at a same-numbered sibling — the
    # duplicate-claim case adr-number-unique also reports). Reads the map the
    # helper just filled; an ADR's rank is its number.
    local file base num index
    while IFS= read -r file; do
        base="$(basename "$file")"
        # Reads the number out of the name; `adr_files` decides membership.
        [[ "$base" =~ ^([0-9]{4})- ]] || continue
        num="${BASH_REMATCH[1]}"
        index=$((10#$num))
        if [[ -z "${REGISTRY_ROW_TARGET[$index]:-}" ]]; then
            violate adr-index-sync "$file" \
                "no index row in ADR/README.md for ADR number $num — every ADR adds exactly one row at its numeric position (see the comment above the index)"
        elif [[ "${REGISTRY_ROW_TARGET[$index]}" != "$base" ]]; then
            violate adr-index-sync "$file" \
                "index row $num links to ./${REGISTRY_ROW_TARGET[$index]}, not to this file — two files are claiming the same ADR number, or the row was not updated with a rename"
        fi
    done < <(adr_files)
}

# ---------------------------------------------------------------------------
# Gate 5d: adr-length
# ---------------------------------------------------------------------------

# docs/CONVENTIONS.md caps an ADR at 150 lines: past that, the file has grown
# a design document, and that body belongs in docs/architecture/ with the
# ADR pointing at it. The cap applies to every ADR except those listed in
# ADR/.length-baseline — the violators that predate the gate, one NNNN per
# line, `#` comments allowed. The baseline only shrinks: a listed ADR that
# fits the cap fails until its entry is removed, so the allowlist cannot
# quietly become a permanent exemption, and an entry naming an ADR that no
# longer exists fails for the same reason. Lines are counted the way an
# editor shows them (a final line without a newline still counts).

ADR_LENGTH_CAP=150

gate_adr_length() {
    if [[ ! -d "$ROOT/ADR" ]]; then
        return
    fi
    local baseline="$ROOT/ADR/.length-baseline"

    # Baseline entries, indexed by ADR number (Bash 3.2: no associative
    # arrays; `10#` so 0008 is not read as octal).
    local -a baselined=()
    local raw entry index
    if [[ -f "$baseline" ]]; then
        while IFS= read -r raw || [[ -n "$raw" ]]; do
            raw="${raw%$'\r'}"
            entry="$(trim "${raw%%#*}")"
            [[ -z "$entry" ]] && continue
            if ! [[ "$entry" =~ ^[0-9]{4}$ ]]; then
                violate adr-length "$baseline" \
                    "entry '$entry' is not a four-digit ADR number"
                continue
            fi
            index=$((10#$entry))
            baselined[$index]="$entry"
        done < "$baseline"
    fi

    local file base num lines
    local -a seen=()
    while IFS= read -r file; do
        base="$(basename "$file")"
        # `adr_files` decides membership; this only reads the number out.
        [[ "$base" =~ ^([0-9]{4})- ]] || continue
        num="${BASH_REMATCH[1]}"
        index=$((10#$num))
        seen[$index]=1
        lines="$(awk 'END { print NR }' "$file")"
        if [[ -n "${baselined[$index]:-}" ]]; then
            if (( lines <= ADR_LENGTH_CAP )); then
                violate adr-length "$file" \
                    "$lines lines fits the $ADR_LENGTH_CAP-line cap; remove $num from ADR/.length-baseline in the same commit (entries may only be removed)"
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
                    "entry ${baselined[$index]} names no ADR/${baselined[$index]}-*.md — remove it"
            fi
        done
    fi
}

# ---------------------------------------------------------------------------
# Gate 5e: adr-in-force-sync
# ---------------------------------------------------------------------------

# ADR/IN-FORCE.md is the topic-ordered view of the decisions currently in
# force: one `[NNNN](./NNNN-slug.md)` line per live ADR under the topic it
# governs, with Proposed ADRs in a trailing block. The view is hand-curated,
# so it drifts unless it is checked against the corpus. Three checks,
# bidirectional:
#
#   1. every ADR whose Status is Proposed, Accepted, or
#      Accepted (forward-compat) is linked exactly once
#   2. every Superseded or Deprecated ADR is linked zero times
#   3. every `[NNNN](./...)` link resolves to a real file whose name
#      starts with the link's number
#
# A malformed `Status:` line is adr-status's finding; this gate skips it.

# Echoes the first `Status:` line outside the frontmatter of an ADR file,
# with the `Status:` prefix and surrounding whitespace removed.
adr_status_value() {
    local file="$1"
    local close
    close="$(frontmatter_close_line "$file" || true)"
    local start=1
    [[ -n "$close" ]] && start=$((close + 1))
    awk -v start="$start" '
        NR < start { next }
        /^Status:/ {
            sub(/\r$/, "")
            sub(/^Status:[[:space:]]*/, "")
            sub(/[[:space:]]+$/, "")
            print
            exit
        }
    ' "$file" || true
}

gate_adr_in_force_sync() {
    if [[ ! -d "$ROOT/ADR" ]]; then
        return
    fi
    local view="$ROOT/ADR/IN-FORCE.md"
    if [[ ! -f "$view" ]]; then
        violate adr-in-force-sync "$view" \
            "ADR/IN-FORCE.md not found — the decisions-in-force view is required"
        return
    fi

    # Collect links: every `[NNNN](./target)` occurrence, one per grep -o
    # match, so a line carrying two links counts both.
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

        if [[ ! -f "$ROOT/ADR/$target" ]]; then
            violate adr-in-force-sync "$view" \
                "link $num points at ./$target, which does not exist"
        elif [[ "$target" != "$num-"* ]]; then
            violate adr-in-force-sync "$view" \
                "link $num points at ./$target, whose filename does not start with $num-"
        fi
    done < <(grep -oE '\[[0-9]{4}\]\(\./[^)]+\)' "$view" 2>/dev/null || true)

    # Reverse direction: each ADR file's Status decides whether it must
    # appear exactly once or must not appear at all.
    local file base status count
    while IFS= read -r file; do
        base="$(basename "$file")"
        # Reads the number out of the name; `adr_files` decides membership.
        [[ "$base" =~ ^([0-9]{4})- ]] || continue
        num="${BASH_REMATCH[1]}"
        index=$((10#$num))
        count="${link_count[$index]:-0}"
        status="$(adr_status_value "$file")"
        case "$status" in
            Proposed|Accepted|"Accepted (forward-compat)")
                if (( count == 0 )); then
                    violate adr-in-force-sync "$file" \
                        "no link in ADR/IN-FORCE.md for ADR $num (Status: $status) — add one line under the topic it governs, or under Proposed"
                elif (( count > 1 )); then
                    violate adr-in-force-sync "$view" \
                        "ADR $num is linked $count times — every in-force ADR appears exactly once"
                elif [[ "${link_target[$index]}" != "$base" ]]; then
                    violate adr-in-force-sync "$file" \
                        "ADR/IN-FORCE.md links $num to ./${link_target[$index]}, not to this file — two files are claiming the same ADR number, or the link was not updated with a rename"
                fi
                ;;
            "Superseded by ADR-"*|Deprecated)
                if (( count > 0 )); then
                    violate adr-in-force-sync "$view" \
                        "ADR $num is '$status' but still linked — remove its line; only Proposed and Accepted ADRs appear in the in-force view"
                fi
                ;;
            *)
                ;;
        esac
    done < <(adr_files)
}

# ---------------------------------------------------------------------------
# Gate 6: spec-version-sync
# ---------------------------------------------------------------------------

# Two things about docs/spec/CHANGELOG.md, both about the version column:
#
#   1. the table is a well-formed shared registry — no version claimed by two
#      rows, rows strictly descending (newest first);
#   2. the head version (first table row that starts with `| <version> |`)
#      agrees with the PROTOCOL_VERSION constant in
#      crates/phux-protocol/src/lib.rs.
#
# (1) was added after two parallel branches both landed 0.8.0-draft.4: the
# head-row check says nothing about rows further down, so a duplicate one row
# from the top passed clean. See the `check_registry_rows` comment.
#
# CONVENTIONS.md notes the SPEC split is in flight; docs/spec/CHANGELOG.md
# does not yet exist. While that's true, this gate emits a single NOTE
# line and does nothing else — it activates automatically once the file
# appears.

gate_spec_version_sync() {
    local changelog="$ROOT/docs/spec/CHANGELOG.md"
    if [[ ! -f "$changelog" ]]; then
        echo "[spec-version-sync] NOTE: $changelog does not exist yet; gate is dormant until the SPEC split lands." >&2
        return
    fi

    # The registry contract first, so that a PROTOCOL_VERSION parse failure
    # below (which returns early) cannot mask a duplicate row. Version rows
    # carry no link, so there is no link base; ordering is descending — see
    # `registry_rank_spec_version` for the rule and where it comes from.
    check_registry_rows \
        "$changelog" \
        spec-version-sync \
        version \
        '^\|[[:space:]]*([0-9]+\.[0-9]+\.[0-9]+[^|[:space:]]*)[[:space:]]*\|' \
        registry_rank_spec_version \
        descending \
        "" \
        "the newest version goes at the top of the table; if a sibling branch already claimed this version, bump yours"

    if (( ${#REGISTRY_ROW_TARGET[@]} == 0 )); then
        violate spec-version-sync "$changelog" \
            "no version rows matched — the table's shape changed and the registry check is now inert; fix the row pattern in scripts/check-docs.sh"
    fi

    # First table row: a line beginning with `| ` followed by a non-pipe
    # version token, then ` |`. We tolerate leading whitespace.
    local head_version
    head_version="$(awk '
        /^[[:space:]]*\|[[:space:]]*[^|[:space:]]+[[:space:]]*\|/ {
            # Strip leading `|` and surrounding whitespace.
            line = $0
            sub(/^[[:space:]]*\|[[:space:]]*/, "", line)
            # Token is everything up to the next `|`.
            n = index(line, "|")
            if (n > 0) {
                v = substr(line, 1, n - 1)
                sub(/[[:space:]]+$/, "", v)
                # Skip header / separator rows like `| Version |` or `|---|`.
                if (v ~ /^[-: ]+$/) next
                if (tolower(v) == "version") next
                print v
                exit
            }
        }
    ' "$changelog")"

    if [[ -z "$head_version" ]]; then
        violate spec-version-sync "$changelog" \
            "could not parse a head version from the first table row"
        return
    fi

    # Pull PROTOCOL_VERSION out of phux-protocol's lib.rs.
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
        violate spec-version-sync "$lib" \
            "could not parse PROTOCOL_VERSION fields (major/minor/patch)"
        return
    fi

    local code_version="${major}.${minor}.${patch}"

    # The changelog's version may carry a pre-release suffix (e.g.
    # `0.2.0-draft.2`); the code constant won't. Compare the
    # `MAJOR.MINOR.PATCH` head only.
    local changelog_core="${head_version%%-*}"

    if [[ "$changelog_core" != "$code_version" ]]; then
        violate spec-version-sync "$changelog" \
            "head version '$head_version' (core '$changelog_core') disagrees with PROTOCOL_VERSION '$code_version' in crates/phux-protocol/src/lib.rs"
    fi
}

# ---------------------------------------------------------------------------
# Gate 6b: spec-id-unique
# ---------------------------------------------------------------------------

# The spec's allocation tables are registries keyed on the wire ID column:
# the message-ID catalogs in proto.md and L1.md, the L3 metadata frames in
# L3.md, the command-tag table in docs/spec/appendix-reserved.md, and the
# agent event-kind table in L1.md. impl-status resolves each row's NAME
# against a const but never reads the ID, so two branches claiming the same
# ID passed every check — the doc half of phux-ke0c (the const half is now
# closed by the FrameType/CommandTag enums and the assert_unique_tags lists
# in crates/phux-protocol/src/wire/frame/mod.rs). Each table is checked on
# its own file: the same ID legitimately appears in both the proto.md and
# L1.md catalogs, and the L1.md event kinds are a separate namespace from
# the message IDs even where the byte values would not collide.

# Rank for a wire-ID key: the byte value. Accepts the `0x1A` form used by
# every table; `16#` reads the hex digits.
registry_rank_hex_byte() {
    [[ "$1" =~ ^0[xX][0-9A-Fa-f]{1,2}$ ]] || return 1
    local h="${1#0x}"
    h="${h#0X}"
    printf '%d\n' "$((16#$h))"
}

gate_spec_id_unique() {
    local spec="$ROOT/docs/spec"
    [[ -d "$spec" ]] || return

    local hint="allocate an open ID from docs/spec/appendix-reserved.md's reserved ranges and keep rows in ascending ID order; if a sibling branch already claimed this ID, renumber yours"

    # The message catalogs in proto.md and L1.md share one row shape:
    # `| 0x01 | C -> S | `NAME` | reference | status |`. Requiring at least
    # five cells keeps L1.md's three-cell event-kind table out of the
    # message namespace; that table is checked separately below.
    local msg_row='^\|[[:space:]]*(0x[0-9A-Fa-f]{2})[[:space:]]*(\|[^|]*){4}\|'
    local file
    for file in proto.md L1.md; do
        [[ -f "$spec/$file" ]] || continue
        check_registry_rows "$spec/$file" spec-id-unique "message ID" \
            "$msg_row" registry_rank_hex_byte ascending "" "$hint"
        if (( ${#REGISTRY_ROW_TARGET[@]} == 0 )); then
            violate spec-id-unique "$spec/$file" \
                "no message-ID rows matched - the table's shape changed and the registry check is now inert; fix the row pattern in scripts/check-docs.sh"
        fi
    done

    # L3.md's metadata frames live in one table whose rows group by
    # direction rather than running ascending across the whole table
    # (`0x50..=0x54`, then `0xD0..=0xD2`, then `0x55` and `0xD3`), so each
    # direction is its own registry: C->S rows and S->C rows each ascend.
    if [[ -f "$spec/L3.md" ]]; then
        check_registry_rows "$spec/L3.md" spec-id-unique "message ID" \
            '^\|[[:space:]]*(0x[0-9A-Fa-f]{2})[[:space:]]*\|[[:space:]]*C[[:space:]]' \
            registry_rank_hex_byte ascending "" "$hint"
        if (( ${#REGISTRY_ROW_TARGET[@]} == 0 )); then
            violate spec-id-unique "$spec/L3.md" \
                "no client-to-server message-ID rows matched - the table's shape changed and the registry check is now inert; fix the row pattern in scripts/check-docs.sh"
        fi
        check_registry_rows "$spec/L3.md" spec-id-unique "message ID" \
            '^\|[[:space:]]*(0x[0-9A-Fa-f]{2})[[:space:]]*\|[[:space:]]*S[[:space:]]' \
            registry_rank_hex_byte ascending "" "$hint"
        if (( ${#REGISTRY_ROW_TARGET[@]} == 0 )); then
            violate spec-id-unique "$spec/L3.md" \
                "no server-to-client message-ID rows matched - the table's shape changed and the registry check is now inert; fix the row pattern in scripts/check-docs.sh"
        fi
    fi

    # The command-tag table in appendix-reserved.md §2 backticks its ID
    # column: `| `0x07` | `GET_SCREEN` | owner | status |`.
    local reserved="$spec/appendix-reserved.md"
    if [[ -f "$reserved" ]]; then
        check_registry_rows "$reserved" spec-id-unique "command tag" \
            '^\|[[:space:]]*`(0x[0-9A-Fa-f]{2})`' \
            registry_rank_hex_byte ascending "" "$hint"
        if (( ${#REGISTRY_ROW_TARGET[@]} == 0 )); then
            violate spec-id-unique "$reserved" \
                "no command-tag rows matched - the table's shape changed and the registry check is now inert; fix the row pattern in scripts/check-docs.sh"
        fi
    fi

    # L1.md's agent event-kind table is a second registry in the same file,
    # three cells with a backticked kind name: `| 0x00 | `command_started`
    # | payload |`. Its tags mirror the EVENT_TAG_ consts, a separate
    # namespace from the message IDs above.
    if [[ -f "$spec/L1.md" ]]; then
        check_registry_rows "$spec/L1.md" spec-id-unique "event kind" \
            '^\|[[:space:]]*(0x[0-9A-Fa-f]{2})[[:space:]]*\|[[:space:]]*`[a-z_]+`[[:space:]]*\|' \
            registry_rank_hex_byte ascending "" "$hint"
        if (( ${#REGISTRY_ROW_TARGET[@]} == 0 )); then
            violate spec-id-unique "$spec/L1.md" \
                "no event-kind rows matched - the table's shape changed and the registry check is now inert; fix the row pattern in scripts/check-docs.sh"
        fi
    fi
}

# ---------------------------------------------------------------------------
# Gate 7: impl-status
# ---------------------------------------------------------------------------

# docs/spec/ and docs/consumers/ describe behavior before it exists — that is
# what spec-first means. The hazard is that a reader cannot tell which
# sentences are shipped, and the repo has been bitten: tui.md §10 specified a
# `phux capture --record` verb and a server-side output tee in the present
# tense, and neither was ever built.
#
# The contract (docs/CONVENTIONS.md §"Implementation status") is that an
# unbuilt surface carries a status claim naming a code symbol. This gate
# resolves every such claim against the code, in BOTH directions: a
# `spec-only` marker whose symbol now exists fails just as loudly as a
# `shipped` claim whose symbol does not. Stale-in-the-other-direction is the
# drift that actually recurs, because implementing something rarely prompts
# anyone to go re-read the prose that said it was unimplemented.
#
# Two carriers, one vocabulary (`shipped` / `partial` / `spec-only`, plus
# `TBD` for an unallocated reservation):
#
#   1. Catalog-table rows, whose last cell is the status word. Resolved
#      against the wire discriminant constants in phux-protocol.
#   2. Prose sections, which carry an HTML comment naming an arbitrary probe
#      symbol immediately above a reader-visible `> **Status` callout.
#
# The gate cannot catch unmarked prose about an unbuilt feature — nothing
# mechanical can. It catches the claims that are made.

IMPL_STATUS_VOCAB='^(shipped|partial|spec-only|TBD)$'

# The crate tree with comment-only lines stripped, materialized once. Every
# probe then costs one grep of one file instead of a walk of the workspace.
IMPL_CODE_CACHE=""

# Built once by gate_impl_status, never from inside a command substitution —
# a subshell would populate a copy of the variable and fire the cleanup trap
# on its own exit, deleting the file out from under the caller.
impl_code_cache_init() {
    [[ -n "$IMPL_CODE_CACHE" ]] && return 0
    IMPL_CODE_CACHE="$(mktemp "${TMPDIR:-/tmp}/phux-impl-status.XXXXXX")"
    # shellcheck disable=SC2064
    trap "rm -f '$IMPL_CODE_CACHE'" EXIT
    # The comment filter is what makes a `spec-only` claim meaningful:
    # `RolePolicy` is named in a phux-protocol doc comment that explains it is
    # unencoded, and a naive grep would read that explanation as evidence of
    # implementation.
    find "$ROOT/crates" -type f -name '*.rs' -not -path '*/target/*' \
        -print0 \
        | xargs -0 cat \
        | grep -vE '^[[:space:]]*(//|/\*|\*)' > "$IMPL_CODE_CACHE" || true
}

trim() {
    local s="$1"
    s="${s#"${s%%[![:space:]]*}"}"
    s="${s%"${s##*[![:space:]]}"}"
    printf '%s' "$s"
}

# Does `probe` appear on a non-comment line of any crate source?
#
# Word boundaries are spelled out rather than using `\b`, which is a GNU
# extension: `TYPE_SUBSCRIBE` must not match `TYPE_SUBSCRIBE_EVENTS`.
probe_matches() {
    local probe="$1"
    grep -qE "(^|[^A-Za-z0-9_])${probe}([^A-Za-z0-9_]|\$)" "$IMPL_CODE_CACHE"
}

# Is there a wire discriminant constant for this message/command name?
# The trailing colon anchors the match to the declaration, so `TYPE_SUBSCRIBE`
# is not satisfied by `TYPE_SUBSCRIBE_EVENTS`.
WIRE_CONSTS=""

wire_const_exists() {
    if [[ -z "$WIRE_CONSTS" ]]; then
        WIRE_CONSTS="$(grep -rhoE 'const (TYPE|COMMAND_TAG)_[A-Z0-9_]+:' \
            "$ROOT/crates/phux-protocol/src/wire" 2>/dev/null || true)"
    fi
    printf '%s\n' "$WIRE_CONSTS" \
        | grep -qxE "const (TYPE|COMMAND_TAG)_${1}:"
}

# Carrier 1 — catalog tables under docs/spec/.
impl_status_tables() {
    local file line row last name probe_ok fenced
    local name_re='`([A-Z][A-Z0-9_]+)`'
    for file in "${FILES[@]}"; do
        case "$file" in
            "$ROOT"/docs/spec/*) ;;
            *) continue ;;
        esac
        # The changelog narrates past releases in prose; its rows are not
        # status claims about the current tree.
        [[ "$file" == "$ROOT/docs/spec/CHANGELOG.md" ]] && continue

        fenced=0
        while IFS= read -r line; do
            # Wire-body listings inside fences are illustrations, not claims.
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

            name=""
            if [[ "$row" =~ $name_re ]]; then
                name="${BASH_REMATCH[1]}"
            fi
            if [[ -z "$name" ]]; then
                violate impl-status "$file" \
                    "table row claims '$last' but names no \`MESSAGE_NAME\` to resolve it against: $row"
                continue
            fi

            probe_ok=0
            wire_const_exists "$name" && probe_ok=1

            case "$last" in
                shipped|partial)
                    if [[ "$probe_ok" -eq 0 ]]; then
                        violate impl-status "$file" \
                            "\`$name\` is marked '$last' but no TYPE_$name / COMMAND_TAG_$name constant exists in crates/phux-protocol/src/wire/ — mark it 'spec-only' or wire it up"
                    fi
                    ;;
                spec-only|TBD)
                    if [[ "$probe_ok" -eq 1 ]]; then
                        violate impl-status "$file" \
                            "\`$name\` is marked '$last' but crates/phux-protocol/src/wire/ declares its discriminant — the codec shipped and the doc did not follow"
                    fi
                    ;;
            esac
        done < "$file"
    done
}

# Carrier 2 — prose markers, anywhere in the doc tree, plus the reverse rule
# that a `> **Status` callout under docs/spec/ or docs/consumers/ must have a
# marker behind it. An unchecked status claim is exactly what this mechanism
# exists to retire.
impl_status_markers() {
    local file line lineno status probes probe
    local marker_re='^<!--[[:space:]]*impl-status:[[:space:]]*([A-Za-z-]+);[[:space:]]*probe:[[:space:]]*([A-Za-z0-9_,:-]+)[[:space:]]*-->[[:space:]]*$'
    local probe_re='^[A-Za-z_][A-Za-z0-9_:-]*$'
    local scoped pending_marker fenced
    for file in "${FILES[@]}"; do
        scoped=0
        case "$file" in
            "$ROOT"/docs/spec/*|"$ROOT"/docs/consumers/*) scoped=1 ;;
        esac

        lineno=0
        fenced=0
        # `pending_marker` is the status of a marker awaiting its callout;
        # empty means the previous content line was not a marker.
        pending_marker=""
        while IFS= read -r line; do
            lineno=$((lineno + 1))
            line="${line%$'\r'}"

            # A fenced block is showing the reader what a marker looks like
            # (docs/CONVENTIONS.md does exactly this), not making a claim.
            if [[ "$line" == '```'* ]]; then
                fenced=$((1 - fenced))
                continue
            fi
            [[ "$fenced" -eq 1 ]] && continue

            if [[ "$line" =~ $marker_re ]]; then
                status="${BASH_REMATCH[1]}"
                probes="${BASH_REMATCH[2]}"
                if ! [[ "$status" =~ $IMPL_STATUS_VOCAB ]]; then
                    violate impl-status "$file" \
                        "line $lineno: unknown impl-status '$status' (allowed: shipped, partial, spec-only, TBD)"
                    pending_marker="invalid"
                    continue
                fi
                local IFS_SAVE="$IFS"
                IFS=','
                # shellcheck disable=SC2086
                set -- $probes
                IFS="$IFS_SAVE"
                for probe in "$@"; do
                    if ! [[ "$probe" =~ $probe_re ]]; then
                        violate impl-status "$file" \
                            "line $lineno: probe '$probe' is not a plain symbol (letters, digits, '_', ':', '-')"
                        continue
                    fi
                    if probe_matches "$probe"; then
                        if [[ "$status" == "spec-only" || "$status" == "TBD" ]]; then
                            violate impl-status "$file" \
                                "line $lineno: marked '$status' but probe '$probe' matches non-comment code under crates/ — the implementation landed and the prose did not follow"
                        fi
                    else
                        if [[ "$status" == "shipped" || "$status" == "partial" ]]; then
                            violate impl-status "$file" \
                                "line $lineno: marked '$status' but probe '$probe' matches no non-comment code under crates/ — fix the probe or the claim"
                        fi
                    fi
                done
                pending_marker="$status"
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

run_gate() {
    local gate="$1"
    should_run "$gate" || return 0
    case "$gate" in
        frontmatter-present) gate_frontmatter_present ;;
        frontmatter-valid)   gate_frontmatter_valid   ;;
        tldr-present)        gate_tldr_present        ;;
        dead-link)           gate_dead_link           ;;
        adr-status)          gate_adr_status          ;;
        adr-number-unique)   gate_adr_number_unique   ;;
        adr-index-sync)      gate_adr_index_sync      ;;
        adr-length)          gate_adr_length          ;;
        adr-in-force-sync)   gate_adr_in_force_sync   ;;
        spec-version-sync)   gate_spec_version_sync   ;;
        spec-id-unique)      gate_spec_id_unique      ;;
        impl-status)         gate_impl_status         ;;
        *) echo "internal error: unknown gate '$gate'" >&2; exit 2 ;;
    esac
}

for gate in "${ALL_GATES[@]}"; do
    run_gate "$gate"
done

echo "checked ${#FILES[@]} files, ${VIOLATIONS} violations"

if (( VIOLATIONS > 0 )); then
    exit 1
fi
exit 0
