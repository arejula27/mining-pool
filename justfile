data_dir       := justfile_directory() / ".bitcoin-data"
conf           := justfile_directory() / "bitcoin" / "bitcoin.conf"
sv2_tp_conf    := justfile_directory() / "bitcoin" / "sv2-tp.conf"
cli            := "bitcoin-cli -datadir=" + data_dir
pid_file       := justfile_directory() / ".bitcoin-node.pid"
sv2_tp_pid     := justfile_directory() / ".sv2-tp.pid"

# List all available recipes
default:
    @just --list

# ── Node ──────────────────────────────────────────────────────────────────────

# Start bitcoin-node in the background (IPC socket for sv2-tp, RPC for tests)
start:
    #!/usr/bin/env bash
    if ! command -v bitcoin-node &>/dev/null; then
        echo "bitcoin-node not found — run from inside nix develop" >&2
        exit 1
    fi
    mkdir -p {{data_dir}}
    cp {{conf}} {{data_dir}}/bitcoin.conf
    nohup bitcoin-node -datadir={{data_dir}} > {{data_dir}}/bitcoin-node.log 2>&1 &
    echo $! > {{pid_file}}
    echo "Waiting for RPC..."
    until {{cli}} getblockchaininfo > /dev/null 2>&1; do sleep 0.5; done
    {{cli}} createwallet default > /dev/null 2>&1 || {{cli}} loadwallet default > /dev/null 2>&1 || true
    echo "bitcoin-node ready"

# Stop bitcoin-node gracefully via RPC
stop:
    {{cli}} stop || true
    @rm -f {{pid_file}}

# Force-kill bitcoin-node (when RPC is unavailable)
kill:
    @if [ -f {{pid_file}} ]; then kill $(cat {{pid_file}}) 2>/dev/null || true; rm -f {{pid_file}}; fi
    pkill bitcoin-node || true

# Check that bitcoin-node RPC is responding
node-check:
    {{cli}} getblockchaininfo

# Run bitcoin-cli with any arguments (e.g. just cli getblocktemplate '{"rules":["segwit"]}')
cli *args:
    {{cli}} {{args}}

# Mine N blocks to a throwaway address (regtest only)
mine n="1":
    {{cli}} generatetoaddress {{n}} $({{cli}} getnewaddress)

# ── sv2-tp ────────────────────────────────────────────────────────────────────

# Start sv2-tp in the background (requires bitcoin-node already running)
start-sv2-tp:
    #!/usr/bin/env bash
    if ! command -v sv2-tp &>/dev/null; then
        echo "sv2-tp not found — run from inside nix develop" >&2
        exit 1
    fi
    nohup sv2-tp -datadir={{data_dir}} -conf={{sv2_tp_conf}} \
        > {{data_dir}}/sv2-tp.log 2>&1 &
    echo $! > {{sv2_tp_pid}}
    echo "Waiting for sv2-tp on :18447..."
    until ss -tlnp 2>/dev/null | grep -q 18447; do sleep 0.5; done
    echo "sv2-tp ready"

# Stop sv2-tp
stop-sv2-tp:
    @if [ -f {{sv2_tp_pid}} ]; then kill $(cat {{sv2_tp_pid}}) 2>/dev/null || true; rm -f {{sv2_tp_pid}}; fi

# Force-kill sv2-tp
kill-sv2-tp:
    @if [ -f {{sv2_tp_pid}} ]; then kill $(cat {{sv2_tp_pid}}) 2>/dev/null || true; rm -f {{sv2_tp_pid}}; fi
    pkill sv2-tp || true

# Run sv2-tp in the foreground (for manual use / debugging)
sv2-tp:
    sv2-tp -datadir={{data_dir}} -conf={{sv2_tp_conf}}

# ── Full environment ──────────────────────────────────────────────────────────

# Start bitcoin-node and sv2-tp (full environment)
start-all: start start-sv2-tp

# Stop sv2-tp then bitcoin-node
stop-all: stop-sv2-tp stop

# Force-kill everything
kill-all: kill-sv2-tp kill

# ── Pool ──────────────────────────────────────────────────────────────────────

# Generate a fresh SV2 authority keypair — appends to .env
keygen:
    cargo run --manifest-path pool/Cargo.toml --bin keygen --quiet

# Run the pool (sources .env if it exists)
run:
    #!/usr/bin/env bash
    [ -f .env ] && set -a && source .env && set +a
    cargo run --manifest-path pool/Cargo.toml --bin pool

# ── Build & test ──────────────────────────────────────────────────────────────

# Compile-check the Rust code (including test targets)
check:
    cargo check --manifest-path pool/Cargo.toml --tests

# Remove Rust build artifacts
clean:
    cargo clean --manifest-path pool/Cargo.toml

# Run unit tests (no bitcoin-node required)
unit:
    cargo test --manifest-path pool/Cargo.toml --lib -- --test-threads=1

# Run all integration tests (starts and stops the full environment)
test-integration:
    #!/usr/bin/env bash
    just start-all || exit 1
    cargo test --manifest-path pool/Cargo.toml --tests -- --nocapture --test-threads=1
    EXIT=$?
    just stop-all
    exit $EXIT
alias int := test-integration

# Run only the RPC integration tests (bitcoin-node only, no sv2-tp)
test-integration-rpc:
    #!/usr/bin/env bash
    just start || exit 1
    cargo test --manifest-path pool/Cargo.toml --test rpc -- --nocapture --test-threads=1
    EXIT=$?
    just stop
    exit $EXIT
alias int-rpc := test-integration-rpc

# Run only the template-client integration tests (full environment required)
test-integration-tdp:
    #!/usr/bin/env bash
    just start-all || exit 1
    cargo test --manifest-path pool/Cargo.toml --test template_client -- --nocapture --test-threads=1
    EXIT=$?
    just stop-all
    exit $EXIT
alias int-tdp := test-integration-tdp
