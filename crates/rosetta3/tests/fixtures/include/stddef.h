/* `size_t` and `ptrdiff_t`, for every header here. */
#ifndef ROSETTA3_TEST_STDDEF_H
#define ROSETTA3_TEST_STDDEF_H

#include "freestanding.h"

typedef __SIZE_TYPE__ size_t;
typedef __PTRDIFF_TYPE__ ptrdiff_t;
typedef long int wchar_t_;

#ifndef NULL
#define NULL ((void *)0)
#endif

#endif /* ROSETTA3_TEST_STDDEF_H */
