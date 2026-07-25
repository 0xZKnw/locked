#!/usr/bin/env bash
#
# The pitch, expressed as a build failure.
#
# Airlock claims that the crates which run the agent loop, execute its tools and
# drive its sandbox *cannot* open a socket. That claim is worth exactly as much
# as this script: it checks the dependency graph, so the property holds by
# construction rather than by discipline, and a future commit that quietly adds
# an HTTP client to the tool executor fails CI instead of shipping.
#
# Run: scripts/check-egress-isolation.sh
set -euo pipefail

cd "$(dirname "$0")/.."

# Crates that must have no path to any HTTP stack, transitive included.
NET_FREE=(airlock-journal airlock-tools airlock-sandbox airlock-core)

# Crates permitted to reach reqwest. `airlock-egress` implements the two doors;
# `airlock-cli` and `airlock-gui` are front ends that construct those clients and
# implement neither. Every entry here is a reviewed decision — the point of the
# list is that adding one is visible in a diff.
ALLOWED_HTTP_REACH=(airlock-egress airlock-cli airlock-gui)

HTTP_STACKS='^(reqwest|hyper|hyper-util|ureq|isahc|surf|attohttpc|curl|curl-sys|h2|tonic) '

fail() { printf '\n\033[31mFAIL\033[0m  %s\n' "$1" >&2; exit 1; }
ok()   { printf '\033[32mok\033[0m    %s\n' "$1"; }

# ---------------------------------------------------------------------------
# 1. No net-free crate may reach an HTTP stack.
# ---------------------------------------------------------------------------
for crate in "${NET_FREE[@]}"; do
    found=$(cargo tree -p "$crate" --edges normal --prefix none 2>/dev/null \
            | sort -u | grep -E "$HTTP_STACKS" || true)
    if [[ -n "$found" ]]; then
        fail "$crate can reach an HTTP stack:
$(echo "$found" | sed 's/^/        /')

  This crate is supposed to be incapable of opening a socket. Either the new
  dependency is a mistake, or the egress invariant just changed and the README
  needs to say so."
    fi
    ok "$crate has no path to an HTTP stack"
done

# ---------------------------------------------------------------------------
# 2. Only the allowed crates may depend on reqwest.
# ---------------------------------------------------------------------------
reachers=$(cargo tree -i reqwest --edges normal --prefix none 2>/dev/null \
           | awk '{print $1}' | sort -u | grep '^airlock-' || true)

while read -r crate; do
    [[ -z "$crate" ]] && continue
    if [[ ! " ${ALLOWED_HTTP_REACH[*]} " =~ " ${crate} " ]]; then
        fail "$crate depends on reqwest but is not in ALLOWED_HTTP_REACH.

  Airlock's central claim is that every network-capable line lives in one crate.
  Adding another is allowed — but it must be a deliberate, reviewed decision,
  recorded here and in the README."
    fi
done <<< "$reachers"
ok "reqwest is reachable only from: ${ALLOWED_HTTP_REACH[*]}"

# ---------------------------------------------------------------------------
# 3. tokio's `net` feature must not leak into a net-free crate.
#
# tokio is legitimately used for processes, files and timers. The `net` feature
# would hand a net-free crate raw TCP — the dependency name alone would not
# catch it, so check the resolved feature set too.
# ---------------------------------------------------------------------------
for crate in "${NET_FREE[@]}"; do
    if cargo tree -p "$crate" --edges normal --format '{p} {f}' 2>/dev/null \
       | grep -E '^tokio ' | grep -qw 'net'; then
        fail "$crate resolves tokio with the 'net' feature — that is a socket by
  another name. Narrow the feature list on whichever dependency pulls it in."
    fi
done
ok "tokio 'net' feature absent from every net-free crate"

printf '\n\033[32mEgress invariant holds.\033[0m\n'
printf 'Third-party egress: TAP only. Inference: the declared model provider.\n'
printf 'Neither is reachable from the loop, the tools, or the sandbox.\n'
