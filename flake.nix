{
	description = "lottery-pool";

	inputs = {
		nixpkgs.url = "nixpkgs/nixos-25.11";
		flake-utils.url = "github:numtide/flake-utils";
		fenix = {
			url = "github:nix-community/fenix";
			inputs.nixpkgs.follows = "nixpkgs";
		};
	};

	outputs = { self, nixpkgs, flake-utils, fenix }:
		flake-utils.lib.eachDefaultSystem (system:
			let
				rustVersion = "1.90.0";

				pkgs = import nixpkgs {
					inherit system;
					config = { allowUnfree = true; };
				};

				rustToolchain = fenix.packages.${system}.fromToolchainName {
					name = rustVersion;
					sha256 = "sha256-SJwZ8g0zF2WrKDVmHrVG3pD2RGoQeo24MEXnNx5FyuI=";
				};

				# Bitcoin Core binaries: only bitcoin-cli (RPC client) and bitcoin-node
				# (IPC-enabled multiprocess daemon). We exclude bitcoind intentionally —
				# bitcoin-node is a strict superset for our use case.
				bitcoin-core = pkgs.buildEnv {
					name = "bitcoin-core-30.2";
					paths = [];
					postBuild = ''
						mkdir -p $out/bin
						ln -s ${pkgs.bitcoind}/bin/bitcoin-cli $out/bin/bitcoin-cli
						ln -s ${pkgs.bitcoind}/libexec/bitcoin-node $out/bin/bitcoin-node
					'';
				};

				# SV2 Template Provider — pre-built binary from GitHub releases.
				# Connects to bitcoin-node via IPC and serves the Template Distribution
				# Protocol to our pool on port 8442.
				# Only available for x86_64-linux (the primary dev platform).
				sv2-tp = pkgs.stdenv.mkDerivation {
					pname = "sv2-tp";
					version = "1.0.3";

					src = pkgs.fetchurl {
						url = "https://github.com/stratum-mining/sv2-tp/releases/download/v1.0.3/sv2-tp-1.0.3-x86_64-linux-gnu.tar.gz";
						hash = "sha256-NkLVTev2DnN88oKzRuXlU5wuEgkz/U2GTFWmkOeJzXg=";
					};

					# Patch the ELF interpreter and RPATH so the binary runs inside the
					# Nix sandbox without relying on /lib64/ld-linux-x86-64.so.2.
					nativeBuildInputs = [ pkgs.autoPatchelfHook ];
					# libgcc_s.so.1 comes from stdenv.cc.cc.lib; glibc is provided by stdenv.
					buildInputs = [ pkgs.stdenv.cc.cc.lib ];

					# The tarball extracts files at its root (no top-level directory),
					# so unpack manually into a subdirectory and point sourceRoot at it
					# so nix cds into it before running subsequent phases.
					unpackPhase = ''
						runHook preUnpack
						mkdir unpacked
						tar -xzf "$src" -C unpacked
						sourceRoot="unpacked/sv2-tp-1.0.3"
						runHook postUnpack
					'';

					installPhase = ''
						runHook preInstall
						mkdir -p "$out/bin"
						cp bin/sv2-tp "$out/bin/sv2-tp"
						runHook postInstall
					'';

					meta.platforms = [ "x86_64-linux" ];
				};

			in
			{
				devShells.default = pkgs.mkShell {
					buildInputs = [
						# Rust
						rustToolchain.toolchain

						# Bitcoin Core (v30.2) — bitcoin-cli + bitcoin-node (IPC-enabled)
						bitcoin-core

						# SV2 Template Provider
						sv2-tp

						# Task runner
						pkgs.just

						# Build deps
						pkgs.pkg-config
					];

					RUST_BACKTRACE = "1";

					shellHook = ''
						bcli() {
							bitcoin-cli -datadir="$PWD/.bitcoin-data" "$@"
						}
						export -f bcli

'';
				};
			}
		);
}
