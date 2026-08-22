{
  description = "Liquid Glass application icons for Linux";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
      ];
    in
    {
      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-rfc-style);

      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          package = pkgs.rustPlatform.buildRustPackage {
            pname = "liquid-glass-icon";
            version = "0.1.0";
            src = pkgs.lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.wrapGAppsHook4
            ];
            buildInputs = [
              pkgs.gtk4
              pkgs.libadwaita
              pkgs.vulkan-loader
            ];

            postInstall = ''
              install -Dm644 assets/liquid-glass-icon.svg \
                $out/share/icons/hicolor/scalable/apps/io.github.yargc.LiquidGlassIcons.svg
              substitute packaging/liquid-glass-icon.desktop.in \
                $out/share/applications/io.github.yargc.LiquidGlassIcons.desktop \
                --replace-fail '@EXEC@' "$out/bin/liquid-glass-icon-gui"
            '';

            preFixup = ''
              gappsWrapperArgs+=(
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath [ pkgs.vulkan-loader ]}
              )
            '';

            meta = {
              description = "Discover, convert and manage Liquid Glass application icons";
              homepage = "https://github.com/OnurByte/LiquidGlassForLinux";
              license = pkgs.lib.licenses.mit;
              mainProgram = "liquid-glass-icon-gui";
              platforms = pkgs.lib.platforms.linux;
            };
          };
        in
        {
          default = package;
          liquid-glass-icon = package;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${nixpkgs.lib.getExe' self.packages.${system}.default "liquid-glass-icon-gui"}";
        };
      });
    };
}
