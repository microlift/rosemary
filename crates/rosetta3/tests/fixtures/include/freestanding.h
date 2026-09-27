/* The declarations every freestanding header here shares, and the reason they
 * are written the way they are.
 *
 * `extern "C"` is load-bearing on every C declaration, and it is not a style
 * choice.  A C++ fixture that declares `printf` without it imports
 * `__Z6printfPKcz`, which nothing in this tree publishes, so the fixture stops
 * at an unbound import instead of printing.  Built and run.
 *
 * These are *declarations only*.  They are here because this host has no
 * `x86_64-apple-darwin` SDK, so there is no `<cstdio>` to include; every
 * definition comes from the tree at run time (`shims::printf` and friends, and
 * the vendored `libc++abi`).  A header that wanted to *provide* an
 * implementation would be this repository's reimplementation wearing the
 * standard's name, which is the thing `build_cpp`'s `-nostdlibinc` is there to
 * prevent: a fixture asking for `<string>` or `<mutex>` still fails, and should.
 */
#ifndef ROSETTA3_TEST_FREESTANDING_H
#define ROSETTA3_TEST_FREESTANDING_H

#ifdef __cplusplus
#define R3_DECL extern "C"
#else
#define R3_DECL extern
#endif

typedef __SIZE_TYPE__ r3_size_t;

#endif /* ROSETTA3_TEST_FREESTANDING_H */
