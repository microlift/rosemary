# What the bundler vendors and what it leaves to the host, as soname globs.
#
# Anything not matched here is vendored if it can be found in the closure.
{ withWebkit ? false }:
{
  # Part of glibc itself: the bundled glibc stage supplies these, or in host
  # mode the host does. Never copied out of a library's own closure, or one
  # process would end up with two libcs.
  glibc = [
    "ld-linux*" "ld64.so*" "libc.so*" "libm.so*" "libdl.so*" "libpthread.so*"
    "librt.so*" "libutil.so*" "libresolv.so*" "libanl.so*" "libnss_*"
    "libmvec.so*" "libthread_db.so*"
  ];

  # Wanted from the host, which has the right one for its hardware, but a copy
  # goes in lib-fallback/ for hosts that have none (NixOS has no Vulkan loader
  # outside a package's closure). What these link against is fallback too. The launcher searches it after the host's
  # own directories.
  fallback = [
    "libvulkan.so*"
    # The glvnd dispatchers; the vendor drivers behind them stay the host's.
    "libEGL.so*" "libGL.so*" "libGLX.so*" "libGLdispatch.so*" "libOpenGL.so*"
    "libGLESv2.so*" "libGLESv1_CM.so*"
    # What SDL3 opens for a window system. NixOS has none of these outside a
    # package's closure, and a minimal install may not either.
    "libwayland-egl.so*" "libX11.so*" "libXext.so*" "libXcursor.so*" "libXi.so*"
    "libXfixes.so*" "libXrandr.so*" "libXrender.so*" "libXss.so*" "libXtst.so*"
  ] ++ (if withWebkit then [
    # WebKitGTK and GTK link these rather than opening them, so a copy has to
    # be there for hosts without them; the host's still comes first.
    "libgbm.so*" "libdrm.so*" "libsystemd.so*" "libudev.so*" "libdbus-1.so*"
    "libxcb*.so*" "libX11-xcb.so*" "libXinerama.so*"
  ] else [ ]);

  # Tied to the host's hardware, drivers, sound servers or window system, so a
  # copy from Nix would only get in the way of the host's own. GL and Vulkan in
  # particular find their vendor drivers through paths nixpkgs patches.
  host = [
    "libGL.so*" "libGLX*" "libGLdispatch*" "libEGL*" "libGLES*" "libOpenGL*"
    "libgbm*" "libdrm*" "libwayland-egl*"
    "libX11*" "libX11-xcb*" "libXext*" "libXcursor*" "libXi.so*" "libXfixes*"
    "libXrandr*" "libXrender*" "libXss*" "libXinerama*" "libXtst*"
    "libXxf86vm*" "libXau*" "libXdmcp*" "libxcb*"
    "libasound*" "libpipewire*" "libjack*" "libsndio*"
    "libudev*" "libdbus-1*" "libsystemd*" "libdecor*" "libibus*"
    "libxkbcommon-x11*"
  ] ++ (if withWebkit then [ ] else [
    # Flite, WebKit's speech synthesizer, links libpulse-simple, which links
    # libpulse and its private libpulsecommon. With WebKit all three are
    # vendored as one set: mixed with the host's, as a host-first fallback
    # allows, they fail on symbols an older host copy lacks. The client
    # protocol is stable, so the set talks to any PulseAudio or pipewire-pulse.
    "libpulse*"
    # Left to the host's GTK stack; Rosemary reports the library missing when
    # the host has none.
    "libwebkitgtk-*" "libjavascriptcoregtk-*"
  ]);
}
