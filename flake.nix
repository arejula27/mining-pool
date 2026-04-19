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

				# SV2 Translator — bridges SV1 miners (Bitaxe/NerdAxe) to our SV2 pool.
				# Statically linked musl binary; no ELF patching needed.
				translator-sv2 = pkgs.stdenv.mkDerivation {
					pname = "translator-sv2";
					version = "0.3.4";

					src = pkgs.fetchurl {
						url = "https://github.com/stratum-mining/sv2-apps/releases/download/v0.3.4/miner-apps-x86_64-unknown-linux-musl.tar.gz";
						hash = "sha256-kREY8TQ8t5CA8MT/h4ncsMd7w9tv6BnYyfU8jFv3r1A=";
					};

					# Extract into a subdirectory and set sourceRoot so nix cds into it.
					unpackPhase = ''
						runHook preUnpack
						mkdir unpacked
						tar -xzf "$src" -C unpacked
						sourceRoot="unpacked/translator"
						runHook postUnpack
					'';

					installPhase = ''
						runHook preInstall
						mkdir -p "$out/bin"
						cp translator_sv2 "$out/bin/translator_sv2"
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

						# SV2 Translator (SV1↔SV2 bridge)
						translator-sv2

						# Task runner
						pkgs.just

						# Build deps
						pkgs.pkg-config
						pkgs.capnproto
						pkgs.sqlite
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
