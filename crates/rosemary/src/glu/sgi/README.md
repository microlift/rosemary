These are the SGI libGLU algorithms from Mesa GLU, vendored from
https://github.com/Distrotech/glu/tree/069211570c32a8d5848e127975a2943e9d8fc6b9
(`src/libutil/{project.c,quad.c,mipmap.c,gluint.h}` and `include/GL/glu.h`).
Their SGI Free Software License B notices are retained in each source file.

`GL/gl.h` supplies only the standard GL ABI subset used by these files and
renames their symbols. `gluos.h` leaves platform selection to SDL. All GL calls
land in Rust functions that use SDL_GL_GetProcAddress; no host GL or GLU library
is linked here. Only the Rust export table determines the guest-visible surface.
The extra upstream mipmap-level/3D helpers and quadric callback registration are
private and are not offered to guests.

Public Rust prototypes and ABI signatures follow Apple's OpenGL/glu.h:
https://github.com/phracker/MacOSX-SDKs/blob/master/MacOSX10.15.sdk/System/Library/Frameworks/OpenGL.framework/Versions/A/Headers/glu.h
Apple's documentation JSON and archived GLU man-page URLs were unavailable when
these were added, so the Apple SDK declarations were used directly.

The upstream registry implementation is not included: extension-token matching
is implemented in Rust to avoid strtok's process-global parser state.

Local changes to `quad.c` initialize the vertex-normal caches for flat point and
silhouette draws, and remove a longitude-cache read indexed by stack count at
the sphere's south pole. These fix undefined reads for valid quadric settings.
