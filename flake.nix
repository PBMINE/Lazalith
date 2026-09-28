{
  description = "Lazalith development workspace";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      packageFor = system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "lazalith-foundations";
          version = "0.1.0";
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./crates ./examples ./docs/isa.md ./docs/boot.md ./docs/lzx.md ./docs/lzo.md ./docs/os-design.md ./docs/os-memory.md ./docs/os-abi.md ./docs/lazen-design.md ./docs/lazen-rationale.md ./docs/lazen-purpose.md ./docs/lazen-syntax.md ./docs/lazen-memory-model.md ./docs/lazen-types.md ./docs/lazen-modules.md ./docs/lazen-applications.md ./docs/lazen-sdk.md ./docs/lazen-graphics.md ./docs/lazen-input.md ./docs/c-compiler.md ./docs/c-runtime.md ./docs/convergence.md ./docs/fuzzing.md ./docs/lazen-packages.md ./docs/lazen-formatting.md ./docs/os-expansion.md ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "--workspace" ];
          cargoTestFlags = [ "--workspace" ];
          doCheck = true;
          # `lazalith-sdl3` reaches SDL3 through pkg-config and compiles a C probe
          # against its headers to check the layout it mirrors. Both need to be in
          # the *build* environment, not only in the dev shell: a package that
          # builds in `nix develop` and not in `nix build` is a package that is
          # broken for everyone who installs it.
          #
          # `PKG_CONFIG_PATH` is set explicitly rather than left to the
          # pkg-config setup hook. The hook would usually do it, and when it does
          # not the failure is a confusing one — pkg-config found, no `sdl3.pc`,
          # in a build that looks correctly configured.
          nativeBuildInputs = [ pkgs.pkg-config pkgs.sdl3 ];
          PKG_CONFIG_PATH = "${pkgs.lib.getDev pkgs.sdl3}/lib/pkgconfig";
          installPhase = ''
            runHook preInstall
            mkdir -p "$out/lib"
            for crate in lazalith_types lazalith_diagnostics lazalith_isa lazalith_cpu lazalith_memory lazalith_devices lazalith_machine lazalith_boot lazalith_os lazalith_os_abi lazalith_toolchain; do
              install -m644 "target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/lib$crate.rlib" "$out/lib/"
            done
            runHook postInstall
          '';
        };
    in
    {
      packages = forAllSystems (system: {
        default = packageFor system;
      });

      checks = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          package = self.packages.${system}.default;
        in
        {
          workspace = package;
          formatting = pkgs.runCommand "lazalith-formatting" {
            nativeBuildInputs = [ pkgs.cargo pkgs.rustfmt ];
            src = package.src;
          } ''
            cp -r "$src" source
            chmod -R u+w source
            cd source
            export HOME="$TMPDIR"
            cargo fmt --all --check
            touch "$out"
          '';
          clippy = package.overrideAttrs (old: {
            pname = "lazalith-clippy";
            nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
            buildPhase = ''
              runHook preBuild
              cargo clippy --offline --locked --workspace --all-targets -- -D warnings
              runHook postBuild
            '';
            doCheck = false;
            installPhase = ''
              touch "$out"
            '';
          });
        });

      devShells = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustc
              cargo
              rustfmt
              clippy
              rust-analyzer
              pkg-config
              cmake
              ninja
              gcc
              gdb
              sdl3
            ];
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
        });
    };
}
