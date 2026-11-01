#pragma once

#include <stddef.h>

#define __WEAK_IMPORT __attribute__((weak))
#define CONSTRUCTOR_FUNCTION __attribute__((constructor))

__WEAK_IMPORT size_t rosemary_host_call_trampoline(void *addr, size_t argcount,
                                                   ...);
__WEAK_IMPORT void *rosemary_host_dlopen(const char *filename, int flags);
__WEAK_IMPORT void *rosemary_host_dlsym(void *handle, const char *symbol);
__WEAK_IMPORT int rosemary_host_dlclose(void *handle);
__WEAK_IMPORT void *rosemary_get_host_rtld_default();
