#!/usr/bin/env bash
# Benchmarks xsh against /bin/bash and /bin/zsh with hyperfine.
#
#   bench/run.sh                 # all workloads
#   bench/run.sh string glob     # only workloads whose name contains a word
#   RUNS=20 WARMUP=3 bench/run.sh
#
# Each workload in bench/workloads runs in every shell. The script first
# checks that all shells print the same output, then times them. Results go
# to target/bench/ (results.md and one JSON file per workload).
set -euo pipefail
cd "$(dirname "$0")/.."

RUNS="${RUNS:-5}"
WARMUP="${WARMUP:-1}"
XSH="${XSH:-target/release/xsh}"
OUT="target/bench"

command -v hyperfine >/dev/null || {
  if command -v brew >/dev/null; then brew install hyperfine; else echo "bench: install hyperfine (https://github.com/sharkdp/hyperfine)" >&2; exit 1; fi
}
[[ -x "$XSH" ]] || cargo build --release -q -p rushy-cli

rm -rf "$OUT" && mkdir -p "$OUT"
export XSH_NO_UPDATE_CHECK=1 HISTFILE=/dev/null

names=(xsh bash zsh)
run_in() {
  case "$1" in
    xsh) printf '%s --norc' "$XSH" ;;
    bash) printf '/bin/bash --norc --noprofile' ;;
    zsh) printf '/bin/zsh -f' ;;
  esac
}

bench() {
  local id="$1" label="$2" arg="$3" ref="" out mismatch="" cmds=()
  for n in "${names[@]}"; do
    out=$(eval "$(run_in "$n") $arg" 2>&1) || true
    if [[ -z "$ref" ]]; then ref="$out"; elif [[ "$out" != "$ref" ]]; then mismatch+="$n "; fi
    cmds+=(-n "$n" "$(run_in "$n") $arg")
  done
  [[ -z "$mismatch" ]] || echo "  ! output differs from xsh in: $mismatch" | tee -a "$OUT/mismatch.txt"
  printf '→ %s\n' "$label"
  hyperfine -N --warmup "$WARMUP" --runs "$RUNS" --export-json "$OUT/$id.json" "${cmds[@]}" >/dev/null 2>"$OUT/$id.err" \
    || { echo "  ! hyperfine failed, see $OUT/$id.err"; return 0; }
  printf '%s\t%s\t%s\n' "$id" "$label" "$mismatch" >> "$OUT/index.tsv"
}

filter=("$@")
want() {
  [[ ${#filter[@]} -eq 0 ]] && return 0
  local w; for w in "${filter[@]}"; do [[ "$1" == *"$w"* ]] && return 0; done; return 1
}

echo "xsh benchmark: $RUNS runs, $WARMUP warm-up, $(sysctl -n machdep.cpu.brand_string), macOS $(sw_vers -productVersion)"
want "00-startup" && bench "00-startup" "startup (-c true)" "-c true"
for f in bench/workloads/*.sh; do
  id=$(basename "$f" .sh)
  want "$id" || continue
  label=$(sed -n '1s/^# *//p' "$f")
  bench "$id" "${id#*-}: $label" "$f"
done

python3 bench/report.py "$OUT" "$("$XSH" --version 2>/dev/null | head -1)"
