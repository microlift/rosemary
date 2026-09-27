/* Declarations only - see freestanding.h.  The definitions are
 * `crates/rosetta3/src/shims/printf.rs` at run time. */
#ifndef ROSETTA3_TEST_STDIO_H
#define ROSETTA3_TEST_STDIO_H

#include "stddef.h"

R3_DECL int printf(const char *, ...);
R3_DECL int puts(const char *);
R3_DECL int putchar(int);
R3_DECL int fputs(const char *, void *);
R3_DECL int fflush(void *);
R3_DECL int snprintf(char *, size_t, const char *, ...);

#endif /* ROSETTA3_TEST_STDIO_H */
