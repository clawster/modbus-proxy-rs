#!/usr/bin/env bash
set -euo pipefail

LLVM_TOOLS_DIR="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | awk '/^host:/{print $2}')/bin"
LLVM_PROFDATA="$LLVM_TOOLS_DIR/llvm-profdata"
LLVM_COV="$LLVM_TOOLS_DIR/llvm-cov"

for tool in "$LLVM_PROFDATA" "$LLVM_COV"; do
    if [[ ! -x "$tool" ]]; then
        echo "Missing $tool — install with: rustup component add llvm-tools-preview" >&2
        exit 1
    fi
done

if ! command -v jq &>/dev/null; then
    echo "Missing jq — install with your package manager (e.g. apt install jq)" >&2
    exit 1
fi

echo "==> Cleaning old profraw/profdata files"
rm -f default_*.profraw coverage.profdata

echo "==> Building and running tests with coverage instrumentation"
RUSTFLAGS="-C instrument-coverage" \
LLVM_PROFILE_FILE="default_%m_%p.profraw" \
    cargo test --tests

echo "==> Merging raw profile data"
$LLVM_PROFDATA merge -sparse default_*.profraw -o coverage.profdata

TEST_BIN=$(
    RUSTFLAGS="-C instrument-coverage" \
    cargo test --tests --no-run --message-format=json 2>/dev/null \
        | jq -r 'select(.profile.test == true) | .filenames[]' \
        | grep -v dSYM \
        | head -1
)

echo "==> Coverage summary"
$LLVM_COV report \
    --ignore-filename-regex='/.cargo/registry|/.rustup/' \
    --instr-profile=coverage.profdata \
    --object "$TEST_BIN" \
    --summary-only

if [[ "${1:-}" == "--show" ]]; then
    echo ""
    echo "==> Detailed line coverage (pipe to less -R for paging)"
    $LLVM_COV show \
        --ignore-filename-regex='/.cargo/registry|/.rustup/' \
        --instr-profile=coverage.profdata \
        --object "$TEST_BIN" \
        --show-line-counts-or-regions \
        --show-instantiations=false \
        --format=text
fi

echo "==> Cleaning up"
rm -f default_*.profraw coverage.profdata
