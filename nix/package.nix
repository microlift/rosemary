{ lib
, stdenv
, craneLib
, pkg-config
, makeBinaryWrapper
, patchelf
, curl
, zlib
, sdl3
, libglvnd
, vulkan-loader
, openal
, webkitgtk_6_0
, libxkbcommon
, wayland
, glib-networking
, gst_all_1
, steamclient
  # "release" or "debug"; debug keeps the extra checks that catch native-guest bugs.
, buildType ? "release"
}:

let
  # Everything here is dlopened by soname at runtime rather than linked, so the
  # linker never records it; it reaches the binary through RUNPATH instead.
  runtimeLibs = [ sdl3 curl zlib ]
    ++ lib.optionals stdenv.hostPlatform.isLinux [
      libglvnd
      vulkan-loader
      openal
      webkitgtk_6_0
      libxkbcommon
      wayland
    ];

  # WebKitGTK's media pipelines. GStreamer looks for plugins only in its own
  # store path (core elements) unless told otherwise, and the web process
  # dies on a page without autoaudiosink and the rest.
  gstPlugins = with gst_all_1; [ gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-libav ];

  # What the crates link or pkg-config against while building. Kept apart from
  # runtimeLibs so that a different SDL3 or OpenAL (nix/bundle.nix wants them
  # built to dlopen their backends) does not rebuild every dependency.
  buildLibs = [ curl zlib ]
    ++ lib.optionals stdenv.hostPlatform.isLinux [ libxkbcommon wayland ];

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      # Patched in through [patch.crates-io] in Cargo.toml.
      ../third_party/libmimalloc-sys
    ];
  };

  # Crane packages every crate of a git dependency to vendor it, and the
  # Cranelift fork has several whose manifests name a README that is not in the
  # repository. An empty one is enough for `cargo package`.
  cargoVendorDir = craneLib.vendorCargoDeps {
    inherit src;
    overrideVendorGitCheckout = packages: drv:
      if lib.any (p: lib.hasPrefix "git+https://github.com/microlift/wasmtime" (p.source or "")) packages
      then drv.overrideAttrs (_: {
        postPatch = ''
          find . -name Cargo.toml | while read -r manifest; do
            dir=$(dirname "$manifest")
            readme=$(sed -n 's/^readme *= *"\(.*\)".*/\1/p' "$manifest" | head -1)
            if [ -n "$readme" ] && [ ! -e "$dir/$readme" ]; then touch "$dir/$readme"; fi
          done
        '';
      })
      else drv;
  };

  common = {
    inherit src cargoVendorDir;
    pname = "rosemary" + lib.optionalString (buildType == "debug") "-debug";
    version = (lib.importTOML ../Cargo.toml).workspace.package.version;
    strictDeps = true;

    # Crane vendors Cargo.lock's git dependencies (Microlift and its Cranelift
    # fork) itself, so there is no hash to bump on every update.
    cargoExtraArgs = "--locked --package rosemary --bin rosemary";
    CARGO_PROFILE = if buildType == "debug" then "dev" else "release";

    nativeBuildInputs = [ pkg-config ];
    buildInputs = buildLibs;

    # The suite needs x86_64 Mach-O fixtures, a GPU and a display.
    doCheck = false;
    # Keep debug info for gdb/lldb and guest symbolication in a debug build.
    dontStrip = buildType == "debug";
  };

  # Third-party crates only: built from a stub of the workspace, so editing
  # Rosemary's own sources never recompiles them. The patched libmimalloc-sys
  # is a third-party crate too, and the stub would strip its API.
  cargoArtifacts = craneLib.buildDepsOnly (removeAttrs common [ "src" ] // {
    dummySrc = craneLib.mkDummySrc {
      inherit src;
      extraDummyScript = ''
        rm -rf $out/third_party/libmimalloc-sys
        cp -r --no-preserve=mode ${src}/third_party/libmimalloc-sys $out/third_party/
      '';
    };
  });
in
craneLib.buildPackage (common // {
  inherit cargoArtifacts;

  nativeBuildInputs = common.nativeBuildInputs
    ++ [ makeBinaryWrapper ]
    ++ lib.optional stdenv.hostPlatform.isLinux patchelf;

  postFixup = lib.optionalString stdenv.hostPlatform.isLinux ''
    patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/rosemary
    # WebKitGTK's network stack finds TLS through a GIO module. A host
    # GStreamer plugin path (NixOS sets one per system) would load plugins
    # built against another GStreamer, so ours replaces it.
    wrapProgram $out/bin/rosemary \
      --prefix GIO_EXTRA_MODULES : ${glib-networking}/lib/gio/modules \
      --set GST_PLUGIN_SYSTEM_PATH_1_0 ${lib.makeSearchPathOutput "lib" "lib/gstreamer-1.0" gstPlugins} \
      --set-default ROSEMARY_STEAM_CLIENT ${steamclient}/lib/steamclient.dylib
  '' + lib.optionalString stdenv.hostPlatform.isDarwin ''
    wrapProgram $out/bin/rosemary \
      --set-default ROSEMARY_STEAM_CLIENT ${steamclient}/lib/steamclient.dylib \
      --prefix DYLD_FALLBACK_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}
  '';

  # The portable packages (nix/bundle.nix) vendor exactly these.
  passthru = { inherit runtimeLibs; };

  meta = {
    description = "Run macOS applications across architectures and operating systems";
    license = with lib.licenses; [ mit asl20 ];
    mainProgram = "rosemary";
    platforms = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
  };
})
