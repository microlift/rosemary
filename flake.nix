{
  description = "Rosemary: run macOS applications across architectures";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay, crane, ... }:
    let
      systems =
        [ "aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
      pkgsFor = system: import nixpkgs {
        inherit system;
        overlays = [ (import rust-overlay) ];
      };
    in {
      packages = forAllSystems (system:
        let
          pkgs = pkgsFor system;
          # nixpkgs' rustc can trail the 1.99 this tree needs.
          toolchain = pkgs.rust-bin.stable.latest.minimal;
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
          steamclient = pkgs.callPackage ./nix/steamclient.nix { };
          rosemary = buildType:
            pkgs.callPackage ./nix/package.nix {
              inherit craneLib buildType steamclient;
            };
          release = rosemary "release";

          # Relocatable packages for non-Nix hosts, built from the same
          # derivation: fhs, then bundle (libraries and glibc vendored in),
          # then each distribution format on top of the bundle.
          #
          # nixpkgs links SDL3 and OpenAL against X11, ALSA, PulseAudio,
          # PipeWire and the rest, so neither loads on a host missing any one
          # of them. Upstream opens those at run time instead, and so does the
          # portable build.
          portableRosemary = pkgs.callPackage ./nix/package.nix {
            inherit craneLib steamclient;
            buildType = "release";
            sdl3 = pkgs.sdl3.overrideAttrs (old: {
              cmakeFlags = (old.cmakeFlags or [ ]) ++ [ "-DSDL_DEPS_SHARED:BOOL=TRUE" ];
              # Its suite wants to open an audio device and a display, which
              # the sandbox has none of once backends are loaded at run time.
              doCheck = false;
            });
            openal = pkgs.openal.overrideAttrs (old: {
              cmakeFlags = (old.cmakeFlags or [ ]) ++ [ "-DALSOFT_DLOPEN:BOOL=TRUE" ];
            });
          };
          portable = rec {
            launcher = pkgs.callPackage ./nix/launcher.nix { };
            fhs = pkgs.callPackage ./nix/fhs.nix {
              rosemary = portableRosemary;
              inherit launcher steamclient;
            };
            bundle = pkgs.callPackage ./nix/bundle.nix {
              rosemary = portableRosemary;
              inherit fhs;
              nixpkgsInfo = {
                inherit (nixpkgs) lastModified;
                rev = nixpkgs.rev or null;
                narHash = nixpkgs.narHash or null;
              };
            };
            steamTool = pkgs.callPackage ./nix/steam-tool.nix {
              rosemary = portableRosemary;
              inherit bundle;
            };
            appimage = pkgs.callPackage ./nix/appimage.nix {
              rosemary = portableRosemary;
              inherit bundle;
            };
          };
        in {
          inherit steamclient;
          # Steam compatibility tool; see tools/steam-compat and
          # homeManagerModules.default (programs.rosemary.steam.enable).
          steam-compat = pkgs.callPackage ./nix/steam-compat.nix {
            rosemary = release;
          };
          default = release;
          rosemary = release;
          rosemary-debug = rosemary "debug";
        } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          rosemary-launcher = portable.launcher;
          rosemary-fhs = portable.fhs;
          rosemary-bundle = portable.bundle;
          rosemary-appimage = portable.appimage;
          rosemary-steam-tool = portable.steamTool;
          rosemary-steam-tool-archive = portable.steamTool.archive;
        });

      # Builds the Steam tool around any rosemary package.
      lib = forAllSystems (system: {
        steamCompat = rosemary:
          (pkgsFor system).callPackage ./nix/steam-compat.nix { inherit rosemary; };
      });

      homeManagerModules.default = import ./nix/home-manager.nix { inherit self; };

      devShells = forAllSystems (system:
        let
          pkgs = pkgsFor system;
          inherit (pkgs) lib;

          toolchain = pkgs.rust-bin.stable.latest.default.override {
            extensions = [ "rust-src" "rust-analyzer" ];
            targets = lib.optional pkgs.stdenv.hostPlatform.isDarwin "x86_64-apple-darwin";
          };

          runtimeLibs = [ pkgs.sdl3 pkgs.curl pkgs.zlib ]
            ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [
              pkgs.libglvnd
              pkgs.vulkan-loader
              # OpenAL: dlopened by the OpenAL.framework bridge.
              pkgs.openal
              # webkit: loaded with dlopen, so it has to be on the library path.
              pkgs.webkitgtk_6_0
              # webkit: the compositor WebKitGTK's windows are shown in.
              pkgs.libxkbcommon
              # NSStatusItem: SDL's tray dlopens an AppIndicator and GTK 3.
              pkgs.libayatana-appindicator
              pkgs.gtk3
            ];
        in {
          default = pkgs.mkShell {
            packages = [
              pkgs.lldb
              # Supply Nix system header paths to clangd, before clang-unwrapped.
              pkgs.clang-tools
              toolchain
              pkgs.pkg-config
              # Cargo fetches microlift and the Cranelift fork over git.
              pkgs.git
              pkgs.python3 # tools/guest-symbolicate.py
              # metal_translate
              pkgs.llvm
              # helpful for debugging metal
              pkgs.vkdevicechooser
              pkgs.vulkan-validation-layers
              pkgs.renderdoc
              # tools/steamclient: cross-compiles the guest-facing SteamClient
              # dylib to Mach-O without a macOS SDK.
              pkgs.llvmPackages.clang-unwrapped
              pkgs.lld
            ];

            # mediabackend's gstreamer feature: base has appsrc, appsink and
            # videoconvert, bad has h264parse, and libav has avdec_h264.
            buildInputs = runtimeLibs ++ [
              pkgs.gst_all_1.gstreamer
              pkgs.gst_all_1.gst-plugins-base
              pkgs.gst_all_1.gst-plugins-bad
              pkgs.gst_all_1.gst-libav
            ];

            # WebKitGTK's network stack finds TLS through a GIO module.
            GIO_EXTRA_MODULES = lib.optionalString pkgs.stdenv.hostPlatform.isLinux
              "${pkgs.glib-networking}/lib/gio/modules";

            LD_LIBRARY_PATH = lib.optionalString pkgs.stdenv.hostPlatform.isLinux
              (lib.makeLibraryPath runtimeLibs);
            DYLD_FALLBACK_LIBRARY_PATH = lib.optionalString pkgs.stdenv.hostPlatform.isDarwin
              (lib.makeLibraryPath runtimeLibs);
            VK_LAYER_PATH="${pkgs.vulkan-validation-layers}/share/vulkan/explicit_layer.d";
          };
        });
    };
}
