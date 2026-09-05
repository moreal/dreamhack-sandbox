{
  description = "wargame-vm: disposable wargame VM orchestrator (Dreamhack + Tart on Apple Silicon)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
      # Tart is Fair Source (unfree in nixpkgs); allow only that package.
      pkgsFor = system: import nixpkgs {
        inherit system;
        config.allowUnfreePredicate = pkg: builtins.elem (nixpkgs.lib.getName pkg) [ "tart" ];
      };
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = pkgsFor system;
          lib = pkgs.lib;
          isDarwin = pkgs.stdenv.isDarwin;
          # Tools resolved from PATH at run time (DESIGN.md §8).
          runtimeDeps = [ pkgs.openssh ] ++ lib.optionals isDarwin [ pkgs.tart pkgs.sshpass ];
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./crates
              ./profiles
              ./guest
            ];
          };
          wargame = pkgs.rustPlatform.buildRustPackage {
            pname = "wargame";
            version = "0.1.0";
            inherit src;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            doCheck = true;
            postInstall = ''
              wrapProgram $out/bin/wargame --prefix PATH : ${lib.makeBinPath runtimeDeps}
            '';
            meta = {
              description = "Disposable wargame VM orchestrator";
              mainProgram = "wargame";
              platforms = systems;
            };
          };
        in
        {
          default = wargame;
          inherit wargame;
        });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/wargame";
        };
      });

      devShells = forAllSystems (system:
        let
          pkgs = pkgsFor system;
          lib = pkgs.lib;
          isDarwin = pkgs.stdenv.isDarwin;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustc
              cargo
              clippy
              rustfmt
              rust-analyzer
              cargo-nextest
              jq
              shellcheck
              openssh
            ] ++ lib.optionals isDarwin [ pkgs.tart pkgs.sshpass ];
            RUST_BACKTRACE = "1";
          };
        });

      checks = forAllSystems (system:
        let
          pkgs = pkgsFor system;
          wargame = self.packages.${system}.default;
        in
        {
          build = wargame;
          fmt = wargame.overrideAttrs (old: {
            pname = "wargame-fmt";
            nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.rustfmt ];
            buildPhase = "cargo fmt --all --check";
            doCheck = false;
            installPhase = "touch $out";
            postInstall = "";
          });
          clippy = wargame.overrideAttrs (old: {
            pname = "wargame-clippy";
            nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
            buildPhase = "cargo clippy --workspace --all-targets -- -D warnings";
            doCheck = false;
            installPhase = "touch $out";
            postInstall = "";
          });
          # `build`'s own `doCheck = true` already runs `cargo test` as part
          # of its checkPhase; this check exists in addition so
          # `nix flake check`'s per-check output names one explicitly
          # (PLAN.md's `checks.{fmt,clippy,tests}`) rather than folding it
          # into `build`.
          tests = wargame.overrideAttrs (old: {
            pname = "wargame-tests";
            buildPhase = "cargo test --workspace";
            doCheck = false;
            installPhase = "touch $out";
            postInstall = "";
          });
        });
    };
}
