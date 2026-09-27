/* Declarations only - see freestanding.h. */
#ifndef ROSETTA3_TEST_STDLIB_H
#define ROSETTA3_TEST_STDLIB_H

#include "stddef.h"

R3_DECL void qsort(void *, size_t, size_t, int (*)(const void *, const void *));
R3_DECL void *malloc(size_t);
R3_DECL void *calloc(size_t, size_t);
R3_DECL void free(void *);
R3_DECL _Noreturn void exit(int);
R3_DECL _Noreturn void abort(void);
R3_DECL int atoi(const char *);
R3_DECL long atol(const char *);
R3_DECL int abs(int);

#endif /* ROSETTA3_TEST_STDLIB_H */
