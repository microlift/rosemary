#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>

#define likely(x) __builtin_expect(!!(x), 1)
#define unlikely(x) __builtin_expect(!!(x), 0)

void __rosemary_stack_chk_failure(const char *msg) {
  fprintf(stderr, "rosemary: stack check failed: %s\n", msg);
  abort();
}

int rosemary_vsnprintf(char *buffer, size_t size, const char *format,
                       va_list ap);
int rosemary_vsprintf(char *buffer, const char *format, va_list ap);

int rosemary_vsprintf_chk(char *s, int flags __attribute__((unused)),
                          size_t slen, const char *format, va_list arg) {
  int done;

  if (unlikely(slen > (size_t)INT_MAX))
    done = rosemary_vsprintf(s, format, arg);
  else {
    done = rosemary_vsnprintf(s, slen, format, arg);
    if (unlikely(done >= 0 && (size_t)done >= slen))
      __rosemary_stack_chk_failure("done >= 0 && (size_t)done >= slen");
  }
  return done;
}

int rosemary_sprintf_chk(char *s, int flags, size_t slen, const char *format,
                         ...) {
  va_list arg;
  va_start(arg, format);
  int done = rosemary_vsprintf_chk(s, flags, slen, format, arg);
  va_end(arg);
  return done;
}

int rosemary_vsnprintf_chk(char *s, size_t maxlen,
                           int flags __attribute__((unused)), size_t slen,
                           const char *format, va_list arg) {
  if (unlikely(slen < maxlen))
    __rosemary_stack_chk_failure("slen < maxlen");
  return rosemary_vsnprintf(s, maxlen, format, arg);
}

int rosemary_snprintf_chk(char *s, size_t maxlen, int flags, size_t slen,
                          const char *format, ...) {
  va_list arg;
  va_start(arg, format);
  int done = rosemary_vsnprintf_chk(s, maxlen, flags, slen, format, arg);
  va_end(arg);
  return done;
}
