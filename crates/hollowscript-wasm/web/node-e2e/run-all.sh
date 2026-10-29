#!/usr/bin/env bash
# Runs every Node end-to-end check of the JavaScript boundary against fresh
# release builds of the WebAssembly modules.
#
#   crates/hollowscript-wasm/web/node-e2e/run-all.sh
#
# What it builds, and where:
#
#   - the VM-only module and the `tooling` module, both `--release` for
#     `wasm32-unknown-unknown` (they write the same file name, so each is
#     copied out before the other is built);
#   - the command line `hollowscript` and the two fixture builders in
#     `crates/hollowscript/examples`, which compile the sample programs to
#     bytecode, as the README in this directory describes.
#
# Everything is built with a Cargo target directory of its own
# (`target/node-e2e` under the repository by default, so a repeat run is
# incremental; `HOLLOWSCRIPT_E2E_TARGET_DIR` names another, a fresh temporary
# one for a cold run), never the one a `cargo test` or another worktree uses,
# so a check can never run against a stale binary. The bytecode and the copies
# of the modules go in a temporary directory that is removed on exit.
#
# Every check must exit 0 *and* print `all checks passed`; a script that exits
# early without saying so is a failure. Needs `node` and the
# `wasm32-unknown-unknown` target; exits 2 with a message when either is
# missing (`scripts/check` skips this step instead of calling it then).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../../.." && pwd)"
cd "$root"

command -v node >/dev/null 2>&1 \
    || { printf 'run-all.sh: node is required\n' >&2; exit 2; }
rustup target list --installed 2>/dev/null | grep -qx wasm32-unknown-unknown \
    || { printf 'run-all.sh: the wasm32-unknown-unknown target is required: rustup target add wasm32-unknown-unknown\n' >&2; exit 2; }

export CARGO_TARGET_DIR="${HOLLOWSCRIPT_E2E_TARGET_DIR:-$root/target/node-e2e}"
work="$(mktemp -d "${TMPDIR:-/tmp}/hollowscript-node-e2e.XXXXXX")"
trap 'rm -rf "$work"' EXIT

step() {
    printf '\n==> %s\n' "$*"
}

wasm_out="$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/hollowscript_wasm.wasm"

step "build the VM-only WebAssembly module"
cargo build --quiet -p hollowscript-wasm --release --target wasm32-unknown-unknown
cp "$wasm_out" "$work/vm.wasm"

step "build the tooling WebAssembly module"
cargo build --quiet -p hollowscript-wasm --release --features tooling --target wasm32-unknown-unknown
cp "$wasm_out" "$work/tooling.wasm"

step "build the command line and the fixture builders"
cargo build --quiet -p hollowscript-cli
cargo build --quiet -p hollowscript --example node_e2e_fixture --example async_e2e_fixture
bin="$CARGO_TARGET_DIR/debug"

step "compile the sample programs to bytecode"
samples="$here"
"$bin/examples/node_e2e_fixture" "$samples/sample.hws" "$work/sample.hsb"
"$bin/examples/async_e2e_fixture" "$samples/async-sample.hws" "$work/async-sample.hsb"
for name in load-result-sample deadline-sample deadline-load-sample results-sample results-load-fault-sample mutation-sample; do
    "$bin/hollowscript" build "$samples/$name.hws" -o "$work/$name.hsb"
done

# An optimised build is shown to keep the memory maximum, when binaryen is here.
optimised=()
if command -v wasm-opt >/dev/null 2>&1; then
    wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int \
        "$work/vm.wasm" -o "$work/vm-optimised.wasm"
    optimised=("$work/vm-optimised.wasm")
else
    printf 'wasm-opt is not installed; the optimised build is not checked (brew install binaryen)\n'
fi

failed=()
run() {
    local name="$1"
    shift
    step "$name"
    local log="$work/$name.log"
    local started=$SECONDS
    if node "$here/$name.mjs" "$@" >"$log" 2>&1 && grep -qx 'all checks passed' "$log"; then
        cat "$log"
        printf '%s: ok (%ss)\n' "$name" "$((SECONDS - started))"
    else
        cat "$log"
        printf '%s: FAILED\n' "$name" >&2
        failed+=("$name")
    fi
}

run main "$work/vm.wasm" "$work/sample.hsb"
run output-check "$work/vm.wasm" "$work/sample.hsb"
run async-check "$work/vm.wasm" "$work/async-sample.hsb"
run deadline-check "$work/vm.wasm" "$work/deadline-sample.hsb" "$work/deadline-load-sample.hsb"
run load-result-check "$work/vm.wasm" "$work/load-result-sample.hsb"
run results-check "$work/vm.wasm" "$work/results-sample.hsb" "$work/results-load-fault-sample.hsb"
run bounds-check "$work/vm.wasm" "$work/tooling.wasm"
run memory-check "$work/vm.wasm" "$work/tooling.wasm" ${optimised[@]+"${optimised[@]}"}
run tooling-check "$work/tooling.wasm"
run ide-check "$work/tooling.wasm"
run deep-nesting-check "$work/tooling.wasm"
run trap-check "$work/tooling.wasm" "$work/vm.wasm"
run instance-churn-check "$work/vm.wasm" "$work/load-result-sample.hsb"
run limits-check "$work/vm.wasm" "$work/deadline-sample.hsb"
run wrapper-hygiene-check "$work/tooling.wasm" "$work/vm.wasm" "$work/load-result-sample.hsb"
run mutation-check "$work/vm.wasm" "$work/mutation-sample.hsb" "$work/deadline-sample.hsb" "$work/sample.hsb"

if [ "${#failed[@]}" -gt 0 ]; then
    printf '\nnode end-to-end checks FAILED: %s\n' "${failed[*]}" >&2
    exit 1
fi
printf '\nAll node end-to-end checks passed.\n'
