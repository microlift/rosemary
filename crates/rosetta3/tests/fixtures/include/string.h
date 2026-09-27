/* Declarations only - see freestanding.h. */
#ifndef ROSETTA3_TEST_STRING_H
#define ROSETTA3_TEST_STRING_H

#include "stddef.h"

R3_DECL void *memcpy(void *, const void *, size_t);
R3_DECL void *memmove(void *, const void *, size_t);
R3_DECL void *memset(void *, int, size_t);
R3_DECL int memcmp(const void *, const void *, size_t);
R3_DECL void *memchr(const void *, int, size_t);
R3_DECL char *strcpy(char *, const char *);
R3_DECL char *strncpy(char *, const char *, size_t);
R3_DECL char *strcat(char *, const char *);
R3_DECL char *strncat(char *, const char *, size_t);
R3_DECL int strcmp(const char *, const char *);
R3_DECL int strncmp(const char *, const char *, size_t);
R3_DECL size_t strlen(const char *);
R3_DECL char *strchr(const char *, int);
R3_DECL char *strrchr(const char *, int);
R3_DECL char *strstr(const char *, const char *);
R3_DECL char *strerror(int);

#endif /* ROSETTA3_TEST_STRING_H */
