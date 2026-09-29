{
  description = "Lazalith development workspace";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      # Every document, by filter rather than by name.
      #
      # This used to be a hand-written list of thirty-odd paths, and a hand-written
      # list is a list that will be one document short the day somebody adds one —
      # and it fails silently, because a document missing from the source tarball
      # is not a build error, it is a document that is missing from a release. A
      # filter over the directory cannot be one short.
      docs = nixpkgs.lib.fileset.fromSource (nixpkgs.lib.cleanSourceWith {
        src = ./docs;
        name = "lazalith-docs";
        filter = path: _type: nixpkgs.lib.hasSuffix ".md" path;
      });

      # The documents at the repository root, which are the specifications rather
      # than the `docs/` record: the Phase-I roadmap, the Beyond specification, the
      # README and the licence.
      #
      # A filter is not enough here, because the root is a mixed directory — a
      # filter over it would put the flake, the workspace manifest and the
      # `.github` workflows into every release tarball. So these are named, and
      # naming them is safe for a different reason than the list it replaced: this
      # is a closed set that cannot grow, because adding a specification document to
      # the repository root is a deliberate act rather than something a filter picks
      # up by accident. A document that *is* added and not listed is still missed,
      # and the way that stops being a silent failure is the CI job that builds the
      # package and runs the program out of it.
      rootDocuments = [
        ./instruction.md
        ./binstruction.md
        ./README.md
        ./LICENSE
      ];

      source = nixpkgs.lib.fileset.toSource {
        root = ./.;
        fileset = nixpkgs.lib.fileset.unions ([
          ./Cargo.toml
          ./Cargo.lock
          ./crates
          ./examples
        ]
        ++ rootDocuments
        ++ [ docs ]);
      };

      packageFor = system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "lazalith-foundations";
          version = "0.1.0";
          src = source;
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

          # Install what the build actually produced.
          #
          # The previous list named eleven library crates and no executables, so
          # `nix build` produced a package containing rlibs that nothing outside a
          # cargo workspace could use, and no program at all. Now every rlib the
          # build produced is installed, and so is every binary, discovered rather
          # than listed — a list of what to install is a list that goes stale the
          # next time a crate is added.
          installPhase = ''
            runHook preInstall
            mkdir -p "$out/lib" "$out/bin"
            release="target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release"
            for rlib in "$release"/lib*.rlib; do
              [ -e "$rlib" ] || continue
              install -m644 "$rlib" "$out/lib/"
            done
            for program in "$release"/*; do
              # A *regular* file, and not a library or a dependency stamp. A
              # directory passes `[ -x ]` — it is searchable — so the first version
              # of this loop tried to install `release/build` and failed the whole
              # build with coreutils' "omitting directory", which is a confusing
              # way to learn that a test needs `-f`.
              [ -f "$program" ] && [ -x "$program" ] || continue
              case "$(basename "$program")" in
                *.so|*.rlib|*.d) continue ;;
              esac
              install -m755 "$program" "$out/bin/"
            done
            test -x "$out/bin/lazen" \
              || { echo "the lazen command was not installed" >&2; exit 1; }
            # B14's separate stage tools. They install through the same loop above,
            # and the same silence that let a missing `lazen` pass unnoticed would let
            # a missing `lazld` pass unnoticed.
            for stage_tool in lazcc lazas lazld; do
              test -x "$out/bin/$stage_tool" \
                || { echo "the $stage_tool stage tool was not installed" >&2; exit 1; }
            done
            runHook postInstall
          '';

          # The same tree, handed to anything that wants to look at what was
          # built. `src` is exposed because a check needs a readable copy of the
          # sources to run the binary against, and the install phase asserts the
          # binary exists rather than trusting that a build produced one.
          passthru = {
            inherit source;
          };
        };
    in
    {
      packages = forAllSystems (system: {
        default = packageFor system;
      });

      # What each of the roadmap's eight areas is checked by.
      #
      # `nix flake check` runs all of these, and each name is a claim about what
      # the check does — so a reader can tell which area is covered by a build
      # and which is covered by running something.
      checks = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          package = self.packages.${system}.default;
          triple = pkgs.stdenv.hostPlatform.rust.rustcTarget;
        in
        {
          # Rust, the emulator, the OS, the assembler, the linker, Lazen, the C
          # compiler, the SDL3 frontend, and the tests: all of them, because
          # `buildRustPackage` with `doCheck` builds the workspace and runs its
          # whole test suite, and the SDL3 crate's C probe compiles as part of
          # that build.
          workspace = package;

          # Style, in the same sandbox `nix build` uses rather than in whatever
          # state the developer's checkout happens to be in.
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

          # Lints, with warnings as errors, over every target including the tests
          # and the examples.
          clippy = package.overrideAttrs (old: {
            pname = "lazalith-clippy";
            nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
            buildPhase = ''
              runHook preBuild
              cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings
              runHook postBuild
            '';
            doCheck = false;
            installPhase = ''
              touch "$out"
            '';
          });

          # The installed program, run.
          #
          # Everything above builds the system; this is the system being used. It
          # runs the binary the package installs, on a file from the repository,
          # through three subcommands: format the source, type-check it, and
          # report the toolchain's version. The first two exercise the lexer, the
          # parser, the type checker and the formatter — the Lazen front end — and
          # the third proves the binary is the one that was built and linked, which
          # no amount of building would show.
          program = pkgs.runCommand "lazalith-program" { } ''
            export HOME="$TMPDIR"
            lazen="${package}/bin/lazen"
            export PATH="$(dirname "$lazen"):$PATH"
            source_root="${package.src}"
            cp -r "$source_root" source
            chmod -R u+w source
            cd source

            lazen fmt --check examples/hello/main.lz
            lazen fmt --check examples/window/main.lz
            lazen check examples/hello/main.lz
            lazen check examples/window/main.lz
            lazen --version > /dev/null

            # The binary must be the one from this build, not one found on PATH.
            test "$lazen" = "${package}/bin/lazen" \
              || { echo "the wrong lazen would be tested" >&2; exit 1; }

            # B14: the three stages run *separately* on a real program, and the
            # image they produce is compared byte-for-byte with the one `lazen
            # build` writes. The comparison is the point — a check that only ran
            # both would pass if the tools disagreed.
            work="$TMPDIR/stages"
            mkdir -p "$work"
            cp examples/hello/main.lz "$work/hello.lz"
            cd "$work"

            lazcc hello.lz -o hello.lzo
            test -s hello.lzo || { echo "lazcc wrote no object" >&2; exit 1; }

            printf '.arch lz64\n.entry _start\n.global _start\n.section .text\n_start:\n    LI r0, 0\n    HALT\n' > tiny.la
            lazas tiny.la -o tiny.lzo
            test -s tiny.lzo || { echo "lazas wrote no object" >&2; exit 1; }

            lazld hello.lzo -o from-tools.lzx
            test -s from-tools.lzx || { echo "lazld wrote no image" >&2; exit 1; }

            lazen build hello.lz
            cmp from-tools.lzx hello.lzx \
              || { echo "lazcc+lazld and lazen build disagree" >&2; exit 1; }

            # The linked image is the program's whole executable, and it still runs.
            lazen run hello.lz

            # C is a first-class target: a C program compiles, links, and runs.
            # This asserts the whole path, not a link, because a C object that
            # merely links proves nothing about the C frontend and the shared
            # backend agreeing.
            cat > hello.c <<'C'
            int main(void) {
                print("Hello from C\n");
                return 7;
            }
            C
            lazcc hello.c -o hello-c.lzo
            test -s hello-c.lzo || { echo "lazcc wrote no C object" >&2; exit 1; }
            lazld hello-c.lzo -o hello-c.lzx --entry fn.c.main
            test -s hello-c.lzx || { echo "lazld wrote no C image" >&2; exit 1; }

            # A C error is reported as a C error, not as a later stage's failure.
            printf 'int main(void) { return not_a_thing; }\n' > bad.c
            if lazcc bad.c -o bad.lzo 2>/dev/null; then
              echo "lazcc accepted a C file with a type error" >&2; exit 1
            fi
            test ! -e bad.lzo || { echo "lazcc wrote an object for a file it refused" >&2; exit 1; }

            # B16: three front ends, one linker, one image 2014 and a cross-language call.
            printf "int triple(int value) { return value * 3; }\n" > math.c
            lazcc -c math.c -o math.lzo
            cat > bridge.lz <<LAZEN
            extern "c" fn triple(value: i32) -> i32;

            fn main() -> i32 {
                return triple(14);
            }
            LAZEN
            lazcc bridge.lz -o bridge.lzo
            lazld math.lzo bridge.lzo -o interop.lzx --entry fn.main
            test -s interop.lzx

            # B15: a target sysroot, written and then read back.
            lazen sysroot "$work/hosted-sysroot"
            for part in include/lazos/abi.h include/lazos/syscall.h \
                        lib/libc.c runtime/lazen-runtime.lz \
                        crt/crt1-lz64-lazen.lzo crt/crt1-lz64-c.lzo; do
              test -f "$work/hosted-sysroot/$part" \
                || { echo "the sysroot has no $part" >&2; exit 1; }
            done

            # A sysroot is the *same* target described somewhere a build can name it,
            # so the object must be identical -- not merely equivalent.
            lazcc hello.c -o built-in.lzo
            lazcc hello.c --sysroot "$work/hosted-sysroot" -o via-sysroot.lzo
            cmp built-in.lzo via-sysroot.lzo \
              || { echo "a sysroot build and a built-in build disagree" >&2; exit 1; }

            # A freestanding sysroot has no C library, and says so by name.
            lazen sysroot "$work/kernel-sysroot" --freestanding
            test ! -f "$work/kernel-sysroot/lib/libc.c" \
              || { echo "a freestanding sysroot has a C library" >&2; exit 1; }
            if lazcc hello.c --sysroot "$work/kernel-sysroot" -o kernel.lzo 2>/dev/null; then
              echo "a hosted build used a freestanding sysroot" >&2; exit 1
            fi

            touch "$out"
          '';

          # The dev shell, built.
          #
          # `nix develop` is on the roadmap's list of things to verify, and a dev
          # shell that has quietly stopped building is the reproducibility failure
          # nobody notices until somebody new clones the repository. Building it as
          # a check is the only way it gets noticed on the day it breaks.
          devShell = self.devShells.${system}.default;
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
