Rosemary for FreeBSD
====================

Built on FreeBSD 14.4 for amd64; it runs on 14.4 and every later release.

Unlike the Linux downloads, this one bundles no libraries. Install the ones
Rosemary links against or loads at run time from packages first:

    pkg install curl sdl3 vulkan-loader mesa-libs

With an NVIDIA card, the nvidia-driver package provides Vulkan and OpenGL in
place of mesa-libs.

To install: extract this directory anywhere and run bin/rosemary inside it,
or link it into a directory on your PATH:

    ln -s "$PWD/Rosemary/bin/rosemary" ~/.local/bin/rosemary

FreeBSD support is newer than the Linux port. Bug reports are welcome:
https://github.com/microlift/rosemary/issues
