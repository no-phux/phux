#!/usr/bin/env bash
# Drift gate for docs/llms.txt and the Build on phux hub.
#
# The hosted file https://docs.phux.sh/llms.txt lives in no-phux/website
# (public/llms.txt). This repository does not generate it. docs/llms.txt is
# the builder index checked in here. The hub's "## Entry points" links and
# this index list the same targets, and every relative target resolves.
#
# Usage: bash scripts/check-llms-txt.sh [--self-test]
# Exit codes: 0 clean, 1 drift, 2 bad invocation.

set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VIOLATIONS=0

usage() {
    cat <<'EOF'
check-llms-txt.sh: docs/llms.txt matches the Build on phux entry points

USAGE:
    bash scripts/check-llms-txt.sh [--help] [--self-test]
EOF
}

violate() {
    phux_error "llms.txt: $1"
    VIOLATIONS=$((VIOLATIONS + 1))
}

# Relative link targets in a "## Entry points" section, anchors kept, one per
# line. Inline code is dropped first so a backticked example is not a link.
entry_links() {
    awk '
        /^## Entry points$/ { grab = 1; next }
        grab && /^## / { exit }
        grab {
            line = $0
            gsub(/`[^`]*`/, "", line)
            while (match(line, /\]\([^) ]+\)/)) {
                target = substr(line, RSTART + 2, RLENGTH - 3)
                if (target !~ /^https?:/ && target !~ /^mailto:/) print target
                line = substr(line, RSTART + RLENGTH)
            }
        }
    ' "$1"
}

# GitHub-style heading anchors, same rules as scripts/check-docs.sh.
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
    ' "$1"
}

# Every relative link in a file, "line target" pairs.
all_links() {
    awk '
        /^[[:space:]]*(```|~~~)/ { fence = !fence; next }
        fence { next }
        {
            line = $0
            gsub(/`[^`]*`/, "", line)
            while (match(line, /\]\([^) ]+\)/)) {
                target = substr(line, RSTART + 2, RLENGTH - 3)
                if (target !~ /^https?:/ && target !~ /^mailto:/) print NR "\t" target
                line = substr(line, RSTART + RLENGTH)
            }
        }
    ' "$1"
}

resolve_target() {
    local from_dir="$1" rel="$2" parent name
    parent="$(cd "$from_dir" && cd "$(dirname "$rel")" && pwd)" || return 1
    name="$(basename "$rel")"
    [[ -n "$name" && -e "$parent/$name" ]] || return 1
    printf '%s/%s\n' "$parent" "$name"
}

check_links() {
    local file="$1" dir lineno link path anchor resolved
    dir="$(dirname "$file")"
    while IFS=$'\t' read -r lineno link; do
        path="${link%%#*}"
        anchor=""
        [[ "$link" == *'#'* ]] && anchor="${link#*#}"
        if [[ -z "$path" ]]; then
            violate "${file#"$ROOT"/}:$lineno empty link"
            continue
        fi
        if ! resolved="$(resolve_target "$dir" "$path")"; then
            violate "${file#"$ROOT"/}:$lineno $link does not exist"
            continue
        fi
        if [[ -n "$anchor" && "$resolved" == *.md ]] &&
            ! heading_anchors "$resolved" | grep -Fqx -- "$anchor"; then
            violate "${file#"$ROOT"/}:$lineno $link has no heading #$anchor"
        fi
    done < <(all_links "$file")
}

# $1 and $2 are files of one target per line. Equal as sets.
same_set() {
    local a b only_a only_b
    a="$(sort -u "$1")"
    b="$(sort -u "$2")"
    only_a="$(comm -23 <(printf '%s\n' "$a") <(printf '%s\n' "$b") || true)"
    only_b="$(comm -13 <(printf '%s\n' "$a") <(printf '%s\n' "$b") || true)"
    [[ -z "$only_a" && -z "$only_b" ]]
}

require_phrase() {
    local file="$1" phrase="$2"
    grep -Fq -- "$phrase" "$file" || violate "${file#"$ROOT"/} is missing: $phrase"
}

forbid_phrase() {
    local file="$1"
    if grep -n -E -i 'no screen scrap|screen-scrap|never scrap|revolutionize|supercharge|seamless|[[:space:]]10x[[:space:]]|unlock' "$file"; then
        violate "${file#"$ROOT"/} has a banned phrase"
    fi
}

check_tree() {
    local root="$1" hub index hub_links index_links stripped required
    hub="$root/docs/building.md"
    index="$root/docs/llms.txt"
    [[ -f "$hub" ]] || violate "missing $hub"
    [[ -f "$index" ]] || violate "missing $index"
    [[ "$VIOLATIONS" -eq 0 ]] || return 0

    check_links "$hub"
    check_links "$index"

    hub_links="$(mktemp)"
    index_links="$(mktemp)"
    stripped="$(mktemp)"
    entry_links "$hub" | sort -u >"$hub_links"
    entry_links "$index" | sort -u >"$index_links"
    grep -Fvx './building.md' "$index_links" >"$stripped" || true
    if ! same_set "$hub_links" "$stripped"; then
        violate "docs/llms.txt Entry points and docs/building.md Entry points differ"
        comm -3 "$hub_links" "$stripped" | sed 's/^/  /' >&2 || true
    fi
    grep -Fqx './building.md' "$index_links" || violate "docs/llms.txt Entry points omit ./building.md"

    required=(
        './scripting.md#json-output-and-errors'
        './scripting.md#watching-events'
        './agents.md#mcp'
        './reference/parity.md'
        './reference/cli.md'
        './protocol.md'
        './compatibility.md'
        './spec/wire-schema.json'
        './program-protocol.md'
        './any-program-status.md'
        './plugins.md'
        './agents.md#agent-setup'
        './agent-quickstart.md#call-phux-from-a-program'
        './embedding.md'
    )
    for link in "${required[@]}"; do
        grep -Fqx -- "$link" "$hub_links" || violate "hub Entry points omit $link"
    done
    rm -f "$hub_links" "$index_links" "$stripped"

    require_phrase "$hub" 'evidence-ranked; screen is the floor'
    require_phrase "$hub" 'It has not been measured on a live Claude Code, Codex, or OpenCode session'
    require_phrase "$hub" 'There is no public SDK.'
    require_phrase "$hub" 'There is no JSON socket.'
    require_phrase "$hub" 'There is no example application.'
    require_phrase "$hub" 'The static plugin index is built from the GitHub topic `phux-plugin`'
    # any-program-status.md is required above. This gate does not require
    # every file under docs/; libghostty.md is not an entry point.
    require_phrase "$index" 'evidence-ranked; screen is the floor'
    require_phrase "$index" 'There is no public SDK.'
    require_phrase "$index" 'There is no JSON socket.'
    require_phrase "$index" 'There is no example application.'
    require_phrase "$index" 'The static plugin index is built from the GitHub topic `phux-plugin`'
    require_phrase "$index" 'https://docs.phux.sh/llms.txt'
    require_phrase "$root/docs/README.md" '](./building.md)'
    require_phrase "$root/README.md" '](./docs/building.md)'
    require_phrase "$root/AGENTS.md" '## Building on phux'
    require_phrase "$root/AGENTS.md" '](./docs/building.md)'

    forbid_phrase "$hub"
    forbid_phrase "$index"
    forbid_phrase "$root/AGENTS.md"
}

self_test() {
    local scratch repo
    scratch="$(mktemp -d "${TMPDIR:-/tmp}/phux-llms-test.XXXXXX")"
    trap 'rm -rf "$scratch"' RETURN
    repo="$scratch/repo"
    mkdir -p "$repo/docs/reference" "$repo/docs/spec" "$repo/examples/agents" \
        "$repo/crates/phux-server/src/runtime/connections" \
        "$repo/integrations/runtime"
    printf '# Scripting\n\n## JSON output and errors\n\n## Watching events\n' >"$repo/docs/scripting.md"
    printf '# Agents\n\n## Agent setup\n\n## MCP\n' >"$repo/docs/agents.md"
    printf '# Parity\n' >"$repo/docs/reference/parity.md"
    printf '# CLI\n\n## `phux watch`\n' >"$repo/docs/reference/cli.md"
    printf '# Protocol\n\n## Transports\n' >"$repo/docs/protocol.md"
    printf '# Compatibility\n\n## Making a change compatible\n' >"$repo/docs/compatibility.md"
    printf '{}\n' >"$repo/docs/spec/wire-schema.json"
    printf '# Program protocol\n' >"$repo/docs/program-protocol.md"
    printf '# Any program can report status\n' >"$repo/docs/any-program-status.md"
    printf '# Sequences\n\n## Precedence\n' >"$repo/docs/program-sequences.md"
    printf '# Plugins\n\n## Managing plugins\n\n## Examples\n' >"$repo/docs/plugins.md"
    printf '# Quickstart\n\n## Call phux from a program\n\n## Run without a terminal\n' >"$repo/docs/agent-quickstart.md"
    printf '# Embed\n' >"$repo/docs/embedding.md"
    printf '# Releasing\n\n## npm and crates.io\n\n## Versioning\n' >"$repo/docs/releasing.md"
    printf '# CLI\n\n## `phux send-keys`\n\n## `phux paste`\n\n## `phux detach`\n\n## `phux kill`\n\n## `phux attach`\n' >>"$repo/docs/reference/cli.md"
    printf 'fn main() {}\n' >"$repo/crates/phux-server/src/agent_state.rs"
    printf 'fn drain_state() {}\n' >"$repo/crates/phux-server/src/runtime/connections/agent_state.rs"
    printf '{}\n' >"$repo/integrations/runtime/package.json"
    printf '# Examples\n' >"$repo/examples/agents/README.md"
    cat >"$repo/docs/building.md" <<'EOF'
# Build on phux

[llms.txt](./llms.txt)

<!-- TODO: the page titled "Any program can report status" is not on main. -->

## Entry points

- [Versioned `--json`](./scripting.md#json-output-and-errors) and the [CLI reference](./reference/cli.md).
- [MCP](./agents.md#mcp) and [parity](./reference/parity.md).
- [`phux watch`](./scripting.md#watching-events).
- [Wire protocol](./protocol.md) and [compatibility](./compatibility.md) and [`wire-schema.json`](./spec/wire-schema.json).
- [OSC 7501](./program-protocol.md).
- [Any program can report status](./any-program-status.md).
- [Plugins](./plugins.md).
- [Agent integrations](./agents.md#agent-setup).
- [Calling phux from a program](./agent-quickstart.md#call-phux-from-a-program) and [embedding](./embedding.md).

## Agent state

Agent state is evidence-ranked; screen is the floor.

It has not been measured on a live Claude Code, Codex, or OpenCode session.

## Not shipped

There is no public SDK.
There is no JSON socket.
There is no example application.
The static plugin index is built from the GitHub topic `phux-plugin`.

[examples](../examples/agents/README.md)
[precedence](./program-sequences.md#precedence)
[plugins manage](./plugins.md#managing-plugins)
[plugins examples](./plugins.md#examples)
[call](./agent-quickstart.md#call-phux-from-a-program)
[run](./agent-quickstart.md#run-without-a-terminal)
[embed](./embedding.md)
[protocol transports](./protocol.md#transports)
[compat change](./compatibility.md#making-a-change-compatible)
[json](./scripting.md#json-output-and-errors)
[npm](./releasing.md#npm-and-cratesio)
[versioning](./releasing.md#versioning)
[watch help](./reference/cli.md#phux-watch)
[send-keys](./reference/cli.md#phux-send-keys)
[paste](./reference/cli.md#phux-paste)
[detach](./reference/cli.md#phux-detach)
[kill](./reference/cli.md#phux-kill)
[attach](./reference/cli.md#phux-attach)
[agent state code](../crates/phux-server/src/agent_state.rs)
[drain](../crates/phux-server/src/runtime/connections/agent_state.rs)
[runtime package](../integrations/runtime/package.json)
EOF
    cat >"$repo/docs/llms.txt" <<'EOF'
# phux

https://docs.phux.sh/llms.txt

## Entry points

- [Build on phux](./building.md)
- [Versioned `--json`](./scripting.md#json-output-and-errors) and the [CLI reference](./reference/cli.md).
- [MCP](./agents.md#mcp) and [parity](./reference/parity.md).
- [`phux watch`](./scripting.md#watching-events).
- [Wire protocol](./protocol.md) and [compatibility](./compatibility.md) and [`wire-schema.json`](./spec/wire-schema.json).
- [OSC 7501](./program-protocol.md).
- [Any program can report status](./any-program-status.md).
- [Plugins](./plugins.md).
- [Agent integrations](./agents.md#agent-setup).
- [Calling phux from a program](./agent-quickstart.md#call-phux-from-a-program) and [embedding](./embedding.md).

Agent state is evidence-ranked; screen is the floor.
There is no public SDK. There is no JSON socket. There is no example application. The static plugin index is built from the GitHub topic `phux-plugin`.
EOF
    printf '[Build on phux](./building.md)\n' >"$repo/docs/README.md"
    printf '[Build on phux](./docs/building.md)\n' >"$repo/README.md"
    printf '## Building on phux\n\n[Build on phux](./docs/building.md)\n' >"$repo/AGENTS.md"

    ROOT="$repo" check_tree "$repo"
    if [[ "$VIOLATIONS" -ne 0 ]]; then
        printf 'self-test: fixture should pass\n' >&2
        exit 1
    fi

    awk '
        !done && $0 == "## Entry points" {
            print
            print ""
            print "- [only hub](./plugins.md#examples)"
            done = 1
            next
        }
        { print }
    ' "$repo/docs/building.md" >"$repo/docs/building.md.new"
    mv "$repo/docs/building.md.new" "$repo/docs/building.md"
    VIOLATIONS=0
    ROOT="$repo" check_tree "$repo" >"$scratch/out" 2>"$scratch/err" || true
    if [[ "$VIOLATIONS" -eq 0 ]]; then
        printf 'self-test: drifted Entry points should fail\n' >&2
        cat "$scratch/err" >&2
        exit 1
    fi
    if ! grep -Fq 'Entry points differ' "$scratch/err"; then
        printf 'self-test: missing drift diagnostic\n' >&2
        cat "$scratch/err" >&2
        exit 1
    fi
}

case "${1:-}" in
    --help|-h) usage; exit 0 ;;
    --self-test) self_test; exit 0 ;;
    "") check_tree "$ROOT" ;;
    *) phux_error "unknown argument: $1"; usage >&2; exit 2 ;;
esac

if [[ "$VIOLATIONS" -ne 0 ]]; then
    exit 1
fi
