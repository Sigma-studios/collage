{
  description = "collage — clipboard manager for KDE Plasma (Wayland): copy toasts, notes, history popup";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # Loaded at runtime with dlopen (GPU backends for the popup).
      runtimeLibs = pkgs: with pkgs; [ wayland libxkbcommon vulkan-loader libGL ];
    in
    {
      packages = forAll (pkgs: rec {
        collage = pkgs.rustPlatform.buildRustPackage {
          pname = "collage";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; [ sqlite wayland libxkbcommon ];

          postFixup = ''
            patchelf --add-rpath ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)} $out/bin/collage
          '';

          meta = {
            description = "Clipboard manager for KDE Plasma on Wayland";
            mainProgram = "collage";
            platforms = pkgs.lib.platforms.linux;
          };
        };
        default = collage;
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.collage ];
          packages = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
        };
      });

      # Home Manager: `imports = [ collage.homeManagerModules.default ];`
      # then `services.collage.enable = true;`
      homeManagerModules.default = { config, lib, pkgs, ... }:
        let
          cfg = config.services.collage;
          toml = pkgs.formats.toml { };
        in
        {
          options.services.collage = {
            enable = lib.mkEnableOption "the collage clipboard daemon";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.stdenv.hostPlatform.system}.collage;
              description = "collage package to use.";
            };
            settings = lib.mkOption {
              type = toml.type;
              default = { };
              example = { history_limit = 1000; toast_duration_ms = 1200; };
              description = "Written to ~/.config/collage/config.toml (see README for keys).";
            };
          };

          config = lib.mkIf cfg.enable {
            home.packages = [ cfg.package ];

            xdg.configFile."collage/config.toml" = lib.mkIf (cfg.settings != { }) {
              source = toml.generate "collage-config.toml" cfg.settings;
            };

            systemd.user.services.collage = {
              Unit = {
                Description = "collage clipboard daemon";
                PartOf = [ "graphical-session.target" ];
                After = [ "graphical-session.target" ];
              };
              Service = {
                ExecStart = "${lib.getExe cfg.package} daemon";
                Restart = "on-failure";
                RestartSec = 2;
              };
              Install.WantedBy = [ "graphical-session.target" ];
            };
          };
        };
    };
}
