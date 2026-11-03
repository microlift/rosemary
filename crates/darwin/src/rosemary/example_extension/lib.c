#include "rosemary_ffi.h"

CONSTRUCTOR_FUNCTION void library_init() {
  void *rtld_default = rosemary_get_host_rtld_default();
  void *host_puts = rosemary_host_dlsym(rtld_default, "puts");
  // call the function through rosemary's exposed trampoline so syscall
  // interposition doesn't catch it
  rosemary_host_call_trampoline(host_puts, 1,
                                "hello from rosemary extension dylib");
}
