#!/usr/bin/env bash
# Model-check one protocol model with TLC: `tools/model.sh <name>` runs every run of models/<name>/
# (the convention is in models/README.md). Fails if a run finds a violation, or a defect run finds none.
set -euo pipefail

name=${1:?usage: tools/model.sh <name>}
root=$(cd "$(dirname "$0")/.." && pwd)
dir=$root/models/$name
[[ -d $dir ]] || { echo "error: no model directory $dir" >&2; exit 2; }

# The pinned TLA+ tools release, fetched into target/ and verified on every run.
url=https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
sha=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88
jar=$root/target/tla/tla2tools.jar
mkdir -p "$root/target/tla"
if [[ ! -f $jar ]]; then
    curl -fsSL -o "$jar.part" "$url"
    mv "$jar.part" "$jar"
fi
if ! echo "$sha  $jar" | shasum -a 256 -c --status; then
    echo "error: $jar does not match SHA-256 $sha; delete it to fetch it again" >&2
    exit 1
fi

out=$root/target/tla/$name
failed=0

# run <cfg> <expect: pass|defect>
run() {
    local cfg=$1 expect=$2 base meta logf code start secs bounds stats verdict
    base=$(basename "$cfg" .cfg)
    [[ $expect == defect ]] && base=defects/$base
    meta=$out/$base
    logf=$meta.log
    rm -rf "$meta"
    mkdir -p "$meta/spec"
    # TLC 2.19 ignores -DTLA-Library, so a defect module finds the modules it extends
    # by being checked next to copies of them.
    cp "$dir"/*.tla "${cfg%.cfg}.tla" "$cfg" "$meta/spec/"
    bounds=$(sed -n 's/^ *\([A-Za-z][A-Za-z0-9_]*\) *= *\([^ ]*\).*/\1=\2/p' "$cfg" | paste -sd ' ' -)
    start=$SECONDS
    code=0
    java -XX:+UseParallelGC -cp "$jar" tlc2.TLC -workers auto -metadir "$meta/states" \
        -config "$meta/spec/$(basename "$cfg")" "$meta/spec/$(basename "${cfg%.cfg}").tla" >"$logf" 2>&1 || code=$?
    secs=$((SECONDS - start))
    rm -rf "$meta"
    stats=$(grep -E 'states generated' "$logf" | tail -n 1 || true)
    if [[ $code -eq 0 ]]; then
        stats="$stats $(grep -E '^The depth of the complete state graph search is' "$logf" | grep -oE '[0-9]+' | sed 's/^/depth /' || true)"
    else
        stats="$stats trace of $(grep -cE '^State [0-9]+:' "$logf" || true) states: $(grep -E '^Error: (Invariant|Action property|Temporal properties|Deadlock)' "$logf" | head -n 1 || true)"
    fi
    # A counterexample is TLC exit 11 (deadlock, when the .cfg checks for it), 12 (an
    # invariant) or 13 (a property, action properties included); parse errors and
    # crashes exit with other codes.
    if [[ $expect == pass && $code -eq 0 ]] || [[ $expect == defect && $code -ge 11 && $code -le 13 ]]; then
        verdict=ok
    else
        verdict=FAILED
        failed=1
    fi
    echo "$verdict $name/$base ($expect, TLC exit $code, ${secs}s) bounds: $bounds"
    echo "    $stats"
    [[ $verdict == ok ]] || tail -n 30 "$logf" | sed 's/^/    | /'
    echo "    log: ${logf#"$root"/}"
}

shopt -s nullglob
cfgs=("$dir"/*.cfg)
defects=("$dir"/defects/*.cfg)
[[ ${#cfgs[@]} -gt 0 ]] || { echo "error: no runs (*.cfg) in $dir" >&2; exit 2; }
for cfg in "${cfgs[@]}"; do run "$cfg" pass; done
for cfg in "${defects[@]}"; do run "$cfg" defect; done
exit "$failed"
