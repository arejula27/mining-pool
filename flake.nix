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
			in
			{
				devShells.default = pkgs.mkShell {
					buildInputs = [
						# Rust
						rustToolchain.toolchain

						# Bitcoin Core
						pkgs.bitcoind

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
