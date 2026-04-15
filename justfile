data_dir := justfile_directory() / ".bitcoin-data"
conf     := justfile_directory() / "bitcoin" / "bitcoin.conf"
cli      := "bitcoin-cli -datadir=" + data_dir

# Start bitcoind as a background daemon using the local data dir
start:
    mkdir -p {{data_dir}}
    cp {{conf}} {{data_dir}}/bitcoin.conf
    bitcoind -datadir={{data_dir}} -daemon
    @echo "Waiting for RPC..."
    @until {{cli}} getblockchaininfo > /dev/null 2>&1; do sleep 0.5; done
    @echo "bitcoind ready"

# Stop bitcoind gracefully via RPC
stop:
    {{cli}} stop || true

# Force-kill bitcoind (when RPC is unavailable)
kill:
    pkill bitcoind || true

# Run bitcoin-cli with any arguments  (e.g. just cli getblocktemplate '{"rules":["segwit"]}')
cli *args:
    {{cli}} {{args}}

# Check that bitcoind RPC is responding
node-check:
    {{cli}} getblockchaininfo

# Remove Rust build artifacts
clean:
    cargo clean --manifest-path pool/Cargo.toml

# Compile-check the Rust code
check:
    cargo check --manifest-path pool/Cargo.toml --tests

# Mine N blocks to a throwaway address (for regtest testing)
mine n="1":
    {{cli}} generatetoaddress {{n}} $({{cli}} getnewaddress)

# Run all integration tests (starts and stops bitcoind automatically)
test-integration:
    #!/usr/bin/env bash
    just start || exit 1
    cargo test --manifest-path pool/Cargo.toml --tests -- --nocapture
    EXIT=$?
    just stop
    exit $EXIT
alias int := test-integration

# Run only the RPC integration tests (starts and stops bitcoind automatically)
test-integration-rpc:
    #!/usr/bin/env bash
    just start || exit 1
    cargo test --manifest-path pool/Cargo.toml --test rpc -- --nocapture
    EXIT=$?
    just stop
    exit $EXIT
alias int-rpc := test-integration-rpc
