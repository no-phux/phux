#!/usr/bin/env bash
# Docs gate: the checks that catch a doc drifting away from the code.
#
#   links     every relative link and #anchor in a tracked Markdown file
#             resolves (headings are slugged the way GitHub slugs them)
#   commands  every `phux <command>` and `just <recipe>` in a shell code
#             block names a command the binary has and a recipe the
#             justfiles define
#   crates    docs/architecture.md names every crates/* directory, and
#             names none that is gone
#
# The command list comes from docs/reference/cli.md, which a unit test keeps
# identical to the binary, so this needs no build.
#
# Usage: bash scripts/check-docs.sh [--help] [--list] [--only=<gate>]
# Exit codes: 0 clean, 1 violations, 2 bad invocation.
#
# Bash 3.2 and POSIX awk, so it runs on a stock Mac with no toolchain.

set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ALL_GATES=(links commands crates)
ONLY=""

usage() {
    cat <<'EOF'
check-docs.sh: dead links, dead anchors, and commands that no longer exist

USAGE:
    bash scripts/check-docs.sh [--help] [--list] [--only=<gate>]

OPTIONS:
    --help          show this message and exit
    --list          print the gates and exit
    --only=<gate>   run one gate (links, commands, crates)
EOF
}

for arg in "$@"; do
    case "$arg" in
        --help|-h) usage; exit 0 ;;
        --list) printf '%s\n' "${ALL_GATES[@]}"; exit 0 ;;
        --only=*) ONLY="${arg#--only=}" ;;
        *) phux_error "unknown argument: $arg"; usage >&2; exit 2 ;;
    esac
done
if [[ -n "$ONLY" && " ${ALL_GATES[*]} " != *" $ONLY "* ]]; then
    phux_error "--only=$ONLY is not a gate (${ALL_GATES[*]})"
    exit 2
fi

# Tracked Markdown, minus generated changelogs, vendored notices, the Beads
# database, and the skills package README (its links resolve in the exported
# no-phux/skills repository, not here). Outside a Git checkout (the setup
# tests), every *.md.
collect_files() {
    if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        git -C "$ROOT" ls-files --cached --others --exclude-standard -- '*.md' |
            while IFS= read -r rel; do
                [[ -f "$ROOT/$rel" ]] && printf '%s\n' "$ROOT/$rel"
            done
    else
        find "$ROOT" -name '*.md' -not -path '*/node_modules/*'
    fi | grep -v -E '(^|/)(CHANGELOG|THIRD-PARTY-NOTICES)\.md$|/\.beads/|/node_modules/|/scripts/skills-package/' |
        LC_ALL=C sort
}

FILES=()
while IFS= read -r f; do FILES+=("$f"); done < <(collect_files)

VIOLATIONS=0
violate() {
    phux_error "[$1] ${2#"$ROOT"/}: $3"
    VIOLATIONS=$((VIOLATIONS + 1))
}

# Prints "<line>\t<target>" for every inline link outside code, one per line.
# Inline code spans are dropped before matching, so `[a](b)` in backticks is
# not a link.
extract_links() {
    awk '
        /^[[:space:]]*(```|~~~)/ { fence = !fence; next }
        fence { next }
        {
            line = $0
            gsub(/`[^`]*`/, "", line)
            while (match(line, /\]\([^) ]+( "[^"]*")?\)/)) {
                target = substr(line, RSTART + 2, RLENGTH - 3)
                sub(/ ".*$/, "", target)
                print NR "\t" target
                line = substr(line, RSTART + RLENGTH)
            }
        }
    ' "$1"
}

# Prints the GitHub anchor of every heading in a Markdown file: lowercase,
# punctuation dropped, spaces to hyphens, a -N suffix for repeats.
heading_anchors() {
    awk '
        /^[[:space:]]*(```|~~~)/ { fence = !fence; next }
        fence { next }
        /^#{1,6} / {
            h = $0
            sub(/^#+ +/, "", h)
            sub(/ +#+ *$/, "", h)
            while (match(h, /\[[^]]*\]\([^)]*\)/)) {
                text = substr(h, RSTART + 1, RLENGTH)
                sub(/\]\(.*$/, "", text)
                h = substr(h, 1, RSTART - 1) text substr(h, RSTART + RLENGTH)
            }
            h = tolower(h)
            gsub(/[^a-z0-9 _-]/, "", h)
            gsub(/ /, "-", h)
            if (h in seen) { seen[h]++; print h "-" seen[h] } else { seen[h] = 0; print h }
        }
        /<a (name|id)="[^"]+"/ {
            s = $0
            sub(/.*<a (name|id)="/, "", s)
            sub(/".*/, "", s)
            print s
        }
    ' "$1"
}

has_anchor() {
    heading_anchors "$1" | grep -Fqx -- "$2"
}

# `-e` alone passes `INSTALL.md` for `install.md` on a case-insensitive
# filesystem; Linux CI and GitHub would not.
exact_child() {
    local parent="$1" name="$2" entry
    for entry in "$parent"/* "$parent"/.*; do
        [[ "${entry##*/}" == "$name" && -e "$entry" ]] && return 0
    done
    return 1
}

exists_exact() {
    local path="$1" parent=/ component
    local components=()
    IFS=/ read -r -a components <<<"$path"
    for component in "${components[@]}"; do
        case "$component" in
            ""|.) continue ;;
            ..) parent="$parent/.." ;;
            *)
                exact_child "$parent" "$component" || return 1
                parent="$parent/$component"
                ;;
        esac
    done
    [[ -e "$parent" ]]
}

gate_links() {
    local file dir lineno link path anchor resolved
    for file in "${FILES[@]}"; do
        dir="$(dirname "$file")"
        while IFS=$'\t' read -r lineno link; do
            case "$link" in
                *://*|mailto:*|tel:*) continue ;;
            esac
            path="${link%%#*}"
            path="${path%%\?*}"
            anchor=""
            [[ "$link" == *'#'* ]] && anchor="${link#*#}"
            if [[ -z "$path" ]]; then
                resolved="$file"
            else
                case "$path" in
                    /*) resolved="$ROOT$path" ;;
                    *) resolved="$dir/$path" ;;
                esac
                if ! exists_exact "$resolved"; then
                    violate links "$file" "line $lineno: $link does not exist"
                    continue
                fi
            fi
            if [[ -n "$anchor" && "$resolved" == *.md && -f "$resolved" ]] &&
                ! has_anchor "$resolved" "$anchor"; then
                violate links "$file" "line $lineno: $link has no heading #$anchor"
            fi
        done < <(extract_links "$file")
    done
}

# Top-level commands and every invocation path from the generated reference.
cli_paths() {
    sed -n 's/^## `phux \(.*\)`$/\1/p' "$ROOT/docs/reference/cli.md"
}

# Recipes and aliases from the root justfile and its modules.
just_recipes() {
    awk '
        /^alias [a-z]/ { print $2; next }
        /^[a-z][a-z0-9_-]*( [^:]*)?:([^=]|$)/ { name = $0; sub(/[ :].*/, "", name); print name }
    ' "$ROOT/justfile" "$ROOT"/just/*.just
}

# Prints "<line>\t<tool>\t<word1>\t<word2>" for each phux or just call in a
# sh/bash/shell/console block. A `$ ` prompt is dropped; comments are not
# commands.
extract_commands() {
    awk '
        /^[[:space:]]*```/ {
            if (!fence) { fence = 1; shell = ($0 ~ /```(sh|bash|shell|console|zsh)[[:space:]]*$/) }
            else { fence = 0; shell = 0 }
            next
        }
        !shell { next }
        {
            line = $0
            sub(/^[[:space:]]*\$ /, "", line)
            sub(/#.*/, "", line)
            n = split(line, parts, /[;&|]+|\$\(/)
            for (i = 1; i <= n; i++) {
                seg = parts[i]
                sub(/^[[:space:]]*(sudo |exec |time |env [A-Z_]+=[^ ]* )*/, "", seg)
                while (seg ~ /^[A-Z_][A-Z0-9_]*=[^ ]* /) sub(/^[^ ]* /, "", seg)
                m = split(seg, w, /[[:space:]]+/)
                if (w[1] != "phux" && w[1] != "just") continue
                print NR "\t" w[1] "\t" w[2] "\t" w[3]
            }
        }
    ' "$1"
}

gate_commands() {
    local paths recipes file lineno tool first second
    paths="$(cli_paths)"
    recipes="$(just_recipes)"
    if [[ -z "$paths" ]]; then
        violate commands "$ROOT/docs/reference/cli.md" "no command headings; run \`just docs-gen\`"
        return
    fi
    for file in "${FILES[@]}"; do
        while IFS=$'\t' read -r lineno tool first second; do
            [[ -z "$first" || "$first" == -* || ! "$first" =~ ^[a-z][a-z0-9-]*$ ]] && continue
            if [[ "$tool" == just ]]; then
                grep -Fqx -- "$first" <<<"$recipes" ||
                    violate commands "$file" "line $lineno: \`just $first\` is not a recipe"
                continue
            fi
            [[ "$first" == help ]] && continue
            if ! grep -Fqx -- "$first" <<<"$paths"; then
                violate commands "$file" "line $lineno: \`phux $first\` is not a command"
                continue
            fi
            # A word after a command group must be one of its subcommands,
            # unless the group itself takes arguments.
            if [[ "$second" =~ ^[a-z][a-z-]*$ ]] &&
                grep -q "^$first " <<<"$paths" &&
                ! grep -Fqx -- "$first $second" <<<"$paths" &&
                grep -q "^Usage: phux $first <SUBCOMMAND>" "$ROOT/docs/reference/cli.md"; then
                violate commands "$file" "line $lineno: \`phux $first $second\` is not a command"
            fi
        done < <(extract_commands "$file")
    done
}

check_named_crate() {
    local page="$1" name="$2"
    case "$name" in
        phux-dev) return 0 ;; # The dev-profile executable, not a crate.
        phux-*|quictun|portable-pty-adopt) ;;
        *) return 0 ;;
    esac
    [[ -d "$ROOT/crates/$name" || -d "$ROOT/clients/$name" ]] ||
        violate crates "$page" "$name is not a crate in this tree"
}

# The root-crate table under "Crates" is the maintained inventory.
# Prose and other sections may name standalone client-workspace packages.
gate_crates() {
    local page="$ROOT/docs/architecture.md" dir name named
    [[ -f "$page" ]] || return 0
    named="$(awk -F'`' '
        /^## / { inventory = ($0 == "## Crates"); next }
        inventory && /^[[:space:]]*\|[[:space:]]*`/ {
            if ($2 ~ /^[a-z][a-z0-9-]*$/) print $2
        }
    ' "$page" | sort -u)"
    for dir in "$ROOT"/crates/*/; do
        name="$(basename "$dir")"
        [[ -f "$dir/Cargo.toml" ]] || continue
        grep -Fqx -- "$name" <<<"$named" ||
            violate crates "$page" "crates/$name is not in the crate table"
    done
    while IFS= read -r name; do
        check_named_crate "$page" "$name"
    done <<<"$named"
}

for gate in "${ALL_GATES[@]}"; do
    [[ -n "$ONLY" && "$gate" != "$ONLY" ]] && continue
    "gate_$gate"
done

if (( VIOLATIONS > 0 )); then
    phux_error "docs: $VIOLATIONS problem(s) in ${#FILES[@]} files"
    exit 1
fi
phux_ok "docs: ${#FILES[@]} files, no dead links or stale commands"
