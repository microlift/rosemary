{ lib
, stdenv
, runCommand
, python3
, patchelf
, closureInfo
, writeText
, glib-networking
, xkeyboard-config
, bubblewrap
, xdg-dbus-proxy
, rosemary
, fhs
, nixpkgsInfo ? null
  # Ship WebKitGTK, its helper processes and a TLS module for it. Without it,
  # web views need the host's WebKitGTK 6.0.
, withWebkit ? true
  # Sonames the code builds at run time, which no scan of the binary can see.
, extraSonames ? [ ]
}:

# fhs plus everything it needs from outside: its libraries, found by following
# DT_NEEDED and the sonames it might dlopen (tools/bundle/bundle.py), and a
# glibc for hosts whose own is too old. Nothing here is a Nix store path at run
# time, but the files were all built by Nix, and share/doc/rosemary/SOURCES.yaml
# says which.
let
  libc = stdenv.cc.libc;
  gccLib = stdenv.cc.cc.lib;

  # package.nix declares what Rosemary links and dlopens; this is that list.
  runtimeLibs = lib.filter (p: withWebkit || (p.pname or "") != "webkitgtk") rosemary.runtimeLibs;
  webkit = lib.findFirst (p: (p.pname or "") == "webkitgtk") null rosemary.runtimeLibs;
  webkitHelpers = "${webkit}/libexec/webkitgtk-6.0";

  # WebKitGTK runs its helpers, bwrap and xdg-dbus-proxy from store paths fixed
  # at build time. This library, added to WebKit's own DT_NEEDED, rewrites them
  # when they are spawned; see tools/bundle/webkit-relocate.c.
  relocate = stdenv.mkDerivation {
    pname = "rosemary-webkit-relocate";
    inherit (rosemary) version;
    src = ../tools/bundle/webkit-relocate.c;
    dontUnpack = true;
    buildPhase = ''
      runHook preBuild
      $CC -shared -fPIC -O2 -Wall -Wextra -Werror \
        -DBAKED_LIBEXEC='"${webkitHelpers}"' \
        -DBAKED_BWRAP='"${bubblewrap}/bin/bwrap"' \
        -DBAKED_DBUS_PROXY='"${xdg-dbus-proxy}/bin/xdg-dbus-proxy"' \
        -DLOADER_NAME='"${baseNameOf fhs.hostLoader}"' \
        -Wl,-soname,librosemary-webkit.so -o librosemary-webkit.so $src
      runHook postBuild
    '';
    # Paths WebKit was not built with would never match, and nothing would be
    # rewritten; a different nixpkgs WebKit is the likely cause.
    doCheck = true;
    checkPhase = ''
      for path in ${webkitHelpers} ${bubblewrap}/bin/bwrap ${xdg-dbus-proxy}/bin/xdg-dbus-proxy; do
        grep -qaF "$path" ${webkit}/lib/libwebkitgtk-6.0.so.4 \
          || { echo "libwebkitgtk does not mention $path" >&2; exit 1; }
      done
      grep -qaF ${bubblewrap}/bin/bwrap ${webkit}/lib/libjavascriptcoregtk-6.0.so.1 \
        || { echo "libjavascriptcoregtk does not mention bwrap" >&2; exit 1; }
    '';
    installPhase = ''
      install -Dm755 librosemary-webkit.so $out/lib/librosemary-webkit.so
    '';
    meta = {
      description = "Points a relocated WebKitGTK at its helper processes";
      license = with lib.licenses; [ mit asl20 ];
    };
  };

  webkitExtras = lib.optionals withWebkit [ relocate glib-networking bubblewrap xdg-dbus-proxy ];
  providers = runtimeLibs ++ [ gccLib ] ++ webkitExtras;

  policy = import ./bundle/policy.nix { inherit withWebkit; };
  metadata = import ./bundle/metadata.nix { inherit lib; } (providers ++ [ libc ]);

  config = writeText "rosemary-bundle.json" (builtins.toJSON ({
    binary = "${rosemary}/bin/.rosemary-wrapped";
    version = rosemary.version;
    nixpkgs = nixpkgsInfo;
    # The libraries SDL and friends open by name are in their build inputs, not
    # on any RUNPATH once they load them at run time. Looked in only for names
    # the policy wants vendored.
    search = map (p: "${lib.getLib p}/lib") (providers ++ lib.concatMap
      (p: lib.filter lib.isDerivation (p.buildInputs or [ ])) providers);
    inherit policy metadata;
    extra_sonames = extraSonames;
    interpreter = fhs.hostLoader;
    registration = "${closureInfo { rootPaths = providers ++ [ libc ]; }}/registration";
    glibc = {
      lib = "${lib.getLib libc}/lib";
      store_path = "${lib.getLib libc}";
      version = libc.version;
      # What a process needs from glibc itself. Leaves out the debugging and
      # auditing libraries, and libidn/locale data nothing here uses.
      keep = [
        "ld-linux*" "ld64.so.*" "libc.so.*" "libm.so.*" "libdl.so.*"
        "libpthread.so.*" "librt.so.*" "libutil.so.*" "libresolv.so.*"
        "libanl.so.*" "libnss_files.so.*" "libnss_dns.so.*" "libmvec.so.*"
      ];
    };
  } // lib.optionalAttrs withWebkit {
    extra_libraries = [
      { path = "${relocate}/lib/librosemary-webkit.so"; dest = "lib"; }
      # GIO loads every module in its directory, so this one gets its own.
      { path = "${glib-networking}/lib/gio/modules/libgiognutls.so"; dest = "lib/gio/modules"; }
      # What the web process loads to run the API's side of a page.
      {
        path = "${webkit}/lib/webkitgtk-6.0/injected-bundle/libwebkitgtkinjectedbundle.so";
        dest = "lib/webkitgtk-6.0/injected-bundle";
      }
    ];
    executables = map (path: { inherit path; dest = "libexec/webkitgtk-6.0"; }) [
      "${webkitHelpers}/WebKitWebProcess"
      "${webkitHelpers}/WebKitNetworkProcess"
      "${webkitHelpers}/WebKitGPUProcess"
      # For hosts without their own; the shim prefers the host's.
      "${bubblewrap}/bin/bwrap"
      "${xdg-dbus-proxy}/bin/xdg-dbus-proxy"
    ];
    add_needed = {
      "libwebkitgtk-6.0.so.4" = [ "librosemary-webkit.so" ];
      "libjavascriptcoregtk-6.0.so.1" = [ "librosemary-webkit.so" ];
    };
    conf = [
      "default_env.GIO_EXTRA_MODULES=@ROOT@/lib/gio/modules"
      "default_env.WEBKIT_INJECTED_BUNDLE_PATH=@ROOT@/lib/webkitgtk-6.0/injected-bundle"
      # libxkbcommon's built-in data directory is a store path.
      "default_env.XKB_CONFIG_ROOT=@ROOT@/share/X11/xkb"
      # The bundled fontconfig and GnuTLS look for their configuration in the
      # store; the host's is wanted, wherever its distribution keeps it.
      "default_path.FONTCONFIG_FILE=/etc/fonts/fonts.conf"
      "default_path.NIX_SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt|/etc/pki/tls/certs/ca-bundle.crt|/etc/ssl/ca-bundle.pem|/etc/ssl/cert.pem"
    ];
  }));
in
runCommand "rosemary-bundle-${rosemary.version}"
  {
    nativeBuildInputs = [ python3 patchelf ];
    __structuredAttrs = true;
    # The libraries are full of their own build paths (glibc's gconv and locale
    # directories, say). Those are never opened: the launcher points the loader
    # at the bundle instead. Listing them as run-time dependencies would
    # defeat the point.
    unsafeDiscardReferences.out = true;
    passthru = { inherit config policy relocate; };
  }
  ''
    cp -r ${fhs} $out
    chmod -R u+w $out
    python3 ${../tools/bundle/bundle.py} --config ${config} --out $out
    # Keymap data for the bundled libxkbcommon; the compositor behind web views
    # cannot build a keyboard without it.
    mkdir -p $out/share/X11
    cp -rL ${xkeyboard-config}/share/X11/xkb $out/share/X11/xkb
    chmod -R u+w $out/share/X11
    test -e $out/share/X11/xkb/rules/evdev
  ''
