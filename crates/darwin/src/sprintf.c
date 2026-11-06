// Darwin's printf rendering, on top of the host's.
//
// Darwin's vfprintf (FreeBSD's) prints a NaN with no sign at all: not the
// sign bit, and not a '+' or ' ' flag. glibc prints "-nan" for a NaN with its
// sign bit set, which is every NaN x86's 0/0 makes. A format that formats a
// NaN is rendered here one conversion at a time, each through the host with
// the sign taken off; every other format goes to the host whole.

#define _GNU_SOURCE // vasprintf

#include <errno.h>
#include <limits.h>
#include <math.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <unistd.h>

// Darwin's arm64 `long double` is `double`; this host's may not be.
#if defined(__aarch64__) && !defined(__APPLE__)
#define LONG_DOUBLE_IS_DOUBLE 1
#else
#define LONG_DOUBLE_IS_DOUBLE 0
#endif

enum kind {
  KIND_NONE,
  KIND_INT,
  KIND_LONG,
  KIND_POINTER,
  KIND_DOUBLE,
  KIND_LONG_DOUBLE,
};

union value {
  int i;
  long long l;
  void *p;
  double d;
  long double ld;
};

enum length { LEN_NONE, LEN_HH, LEN_H, LEN_L, LEN_LL, LEN_BIG_L, LEN_J, LEN_Z, LEN_T };

// A `*` or a literal; `arg` is an index into the arguments when it is a `*`.
struct number {
  bool present;
  int arg;
  const char *digits;
  size_t ndigits;
};

struct conversion {
  const char *start; // the '%'
  const char *end;   // one past the conversion character
  const char *flags;
  size_t nflags;
  struct number width;
  struct number precision;
  enum length length;
  const char *length_text;
  size_t nlength;
  char conv;
  int arg; // the value's index, or -1 for "%%"
};

struct plan {
  struct conversion *conversions;
  size_t nconversions;
  enum kind *kinds;
  union value *values;
  size_t nargs;
};

static void plan_free(struct plan *plan) {
  free(plan->conversions);
  free(plan->kinds);
  free(plan->values);
}

static bool grow(void **items, size_t size, size_t count, size_t *capacity) {
  if (count < *capacity)
    return true;
  size_t next = *capacity ? *capacity * 2 : 16;
  void *moved = realloc(*items, next * size);
  if (!moved)
    return false;
  *items = moved;
  *capacity = next;
  return true;
}

static int parse_digits(const char **p) {
  int n = 0;
  while (**p >= '0' && **p <= '9') {
    if (n <= (INT_MAX - 9) / 10)
      n = n * 10 + (**p - '0');
    (*p)++;
  }
  return n;
}

// `n$`, as a zero-based index, or -1 if `p` does not start one.
static int parse_position(const char **p) {
  const char *q = *p;
  if (*q < '1' || *q > '9')
    return -1;
  int n = parse_digits(&q);
  if (*q != '$')
    return -1;
  *p = q + 1;
  return n - 1;
}

static enum kind kind_of(char conv, enum length length) {
  switch (conv) {
  case 'd':
  case 'i':
  case 'o':
  case 'u':
  case 'x':
  case 'X':
    return length >= LEN_L ? KIND_LONG : KIND_INT;
  case 'c':
  case 'C':
    return KIND_INT;
  case 's':
  case 'S':
  case 'p':
  case 'n':
    return KIND_POINTER;
  case 'a':
  case 'A':
  case 'e':
  case 'E':
  case 'f':
  case 'F':
  case 'g':
  case 'G':
    return length == LEN_BIG_L && !LONG_DOUBLE_IS_DOUBLE ? KIND_LONG_DOUBLE
                                                         : KIND_DOUBLE;
  default:
    return KIND_NONE;
  }
}

static bool is_float(char conv) {
  return conv != '\0' && strchr("aAeEfFgG", conv) != NULL;
}

// A scan that allocates nothing, so a format with no floating-point
// conversion - most of them - costs the host's rendering and no more.
static bool has_float_conversion(const char *format) {
  for (const char *p = format; (p = strchr(p, '%')) != NULL;) {
    p++;
    if (*p == '%') {
      p++;
      continue;
    }
    while (*p && strchr("0123456789$-+ #'*.hlqLjzt", *p))
      p++;
    if (is_float(*p))
      return true;
  }
  return false;
}

// Positional and sequential arguments together are undefined, and the
// positional form leaves an argument's type to whichever conversion names it.
struct numbering {
  int positional; // -1 undecided, 0 sequential, 1 positional
  int next;
  enum kind *kinds;
  size_t capacity;
  size_t count;
};

static bool assign(struct numbering *n, int position, enum kind kind,
                   int *index) {
  int want = position >= 0;
  if (n->positional < 0)
    n->positional = want;
  else if (n->positional != want)
    return false;
  int at = want ? position : n->next++;
  while (n->count <= (size_t)at) {
    if (!grow((void **)&n->kinds, sizeof *n->kinds, n->count, &n->capacity))
      return false;
    n->kinds[n->count++] = KIND_NONE;
  }
  if (n->kinds[at] != KIND_NONE && n->kinds[at] != kind)
    return false;
  n->kinds[at] = kind;
  *index = at;
  return true;
}

static bool parse_number(const char **p, struct numbering *n,
                         struct number *out) {
  out->present = true;
  out->arg = -1;
  if (**p == '*') {
    (*p)++;
    return assign(n, parse_position(p), KIND_INT, &out->arg);
  }
  out->digits = *p;
  parse_digits(p);
  out->ndigits = (size_t)(*p - out->digits);
  return true;
}

static enum length parse_length(const char **p) {
  switch (**p) {
  case 'h':
    (*p)++;
    if (**p == 'h') {
      (*p)++;
      return LEN_HH;
    }
    return LEN_H;
  case 'l':
    (*p)++;
    if (**p == 'l') {
      (*p)++;
      return LEN_LL;
    }
    return LEN_L;
  case 'q':
    (*p)++;
    return LEN_LL;
  case 'L':
    (*p)++;
    return LEN_BIG_L;
  case 'j':
    (*p)++;
    return LEN_J;
  case 'z':
    (*p)++;
    return LEN_Z;
  case 't':
    (*p)++;
    return LEN_T;
  default:
    return LEN_NONE;
  }
}

// False for anything this cannot type, which the host is then left to
// render as it would have.
static bool parse(const char *format, struct plan *plan) {
  struct numbering numbering = {.positional = -1};
  size_t capacity = 0;
  *plan = (struct plan){0};
  for (const char *p = format; (p = strchr(p, '%')) != NULL;) {
    if (!grow((void **)&plan->conversions, sizeof *plan->conversions,
              plan->nconversions, &capacity))
      goto fail;
    struct conversion *c = &plan->conversions[plan->nconversions++];
    *c = (struct conversion){.start = p, .arg = -1};
    p++;
    if (*p == '%') {
      c->conv = *p++;
      c->end = p;
      continue;
    }
    int position = parse_position(&p);
    c->flags = p;
    while (*p && strchr("-+ #0'", *p))
      p++;
    c->nflags = (size_t)(p - c->flags);
    if ((*p == '*' || (*p >= '0' && *p <= '9')) &&
        !parse_number(&p, &numbering, &c->width))
      goto fail;
    if (*p == '.') {
      p++;
      if (!parse_number(&p, &numbering, &c->precision))
        goto fail;
    }
    c->length_text = p;
    c->length = parse_length(&p);
    c->nlength = (size_t)(p - c->length_text);
    c->conv = *p;
    enum kind kind = kind_of(c->conv, c->length);
    if (kind == KIND_NONE || !assign(&numbering, position, kind, &c->arg))
      goto fail;
    c->end = ++p;
  }
  for (size_t i = 0; i < numbering.count; i++)
    if (numbering.kinds[i] == KIND_NONE)
      goto fail;
  plan->kinds = numbering.kinds;
  plan->nargs = numbering.count;
  return true;
fail:
  free(numbering.kinds);
  plan_free(plan);
  *plan = (struct plan){0};
  return false;
}

static bool fetch(struct plan *plan, va_list ap) {
  plan->values = calloc(plan->nargs ? plan->nargs : 1, sizeof *plan->values);
  if (!plan->values)
    return false;
  va_list args;
  va_copy(args, ap);
  for (size_t i = 0; i < plan->nargs; i++) {
    union value *v = &plan->values[i];
    switch (plan->kinds[i]) {
    case KIND_INT:
      v->i = va_arg(args, int);
      break;
    case KIND_LONG:
      v->l = va_arg(args, long long);
      break;
    case KIND_POINTER:
      v->p = va_arg(args, void *);
      break;
    case KIND_DOUBLE:
      v->d = va_arg(args, double);
      break;
    case KIND_LONG_DOUBLE:
      v->ld = va_arg(args, long double);
      break;
    case KIND_NONE:
      break;
    }
  }
  va_end(args);
  return true;
}

static bool formats_a_nan(const struct plan *plan) {
  for (size_t i = 0; i < plan->nconversions; i++) {
    const struct conversion *c = &plan->conversions[i];
    if (c->arg < 0 || !is_float(c->conv))
      continue;
    const union value *v = &plan->values[c->arg];
    if (plan->kinds[c->arg] == KIND_LONG_DOUBLE ? isnan(v->ld) : isnan(v->d))
      return true;
  }
  return false;
}

struct buffer {
  char *data;
  size_t len;
  size_t capacity;
};

static bool reserve(struct buffer *b, size_t more) {
  if (more > SIZE_MAX - b->len - 1)
    return false;
  size_t need = b->len + more + 1;
  if (need <= b->capacity)
    return true;
  size_t next = b->capacity ? b->capacity : 64;
  while (next < need)
    next = next > SIZE_MAX / 2 ? need : next * 2;
  char *moved = realloc(b->data, next);
  if (!moved)
    return false;
  b->data = moved;
  b->capacity = next;
  return true;
}

static bool append(struct buffer *b, const char *text, size_t n) {
  if (!reserve(b, n))
    return false;
  memcpy(b->data + b->len, text, n);
  b->len += n;
  b->data[b->len] = '\0';
  return true;
}

static void store_count(const struct conversion *c, void *at, size_t count) {
  switch (c->length) {
  case LEN_HH:
    *(signed char *)at = (signed char)count;
    break;
  case LEN_H:
    *(short *)at = (short)count;
    break;
  case LEN_L:
    *(long *)at = (long)count;
    break;
  case LEN_LL:
  case LEN_BIG_L:
    *(long long *)at = (long long)count;
    break;
  case LEN_J:
    *(intmax_t *)at = (intmax_t)count;
    break;
  case LEN_Z:
    *(ssize_t *)at = (ssize_t)count;
    break;
  case LEN_T:
    *(ptrdiff_t *)at = (ptrdiff_t)count;
    break;
  case LEN_NONE:
    *(int *)at = (int)count;
    break;
  }
}

// One conversion, as a format of its own: positions dropped, `*` resolved to
// digits, and for a NaN the flags that would sign it left out.
static bool single_format(const struct plan *plan, const struct conversion *c,
                          bool unsigned_nan, char **out) {
  size_t room = (size_t)(c->end - c->start) + 2 * (3 * sizeof(int) + 2) + 1;
  char *f = malloc(room);
  if (!f)
    return false;
  char *w = f;
  *w++ = '%';
  for (size_t i = 0; i < c->nflags; i++) {
    char flag = c->flags[i];
    if (unsigned_nan && (flag == '+' || flag == ' '))
      continue;
    *w++ = flag;
  }
  if (c->width.present) {
    if (c->width.arg >= 0) {
      int width = plan->values[c->width.arg].i;
      // A negative `*` width is a '-' flag and its magnitude.
      if (width < 0) {
        *w++ = '-';
        width = width == INT_MIN ? INT_MAX : -width;
      }
      w += sprintf(w, "%d", width);
    } else {
      memcpy(w, c->width.digits, c->width.ndigits);
      w += c->width.ndigits;
    }
  }
  if (c->precision.present) {
    // A negative `*` precision is as if there were none.
    if (c->precision.arg < 0) {
      *w++ = '.';
      memcpy(w, c->precision.digits, c->precision.ndigits);
      w += c->precision.ndigits;
    } else if (plan->values[c->precision.arg].i >= 0) {
      w += sprintf(w, ".%d", plan->values[c->precision.arg].i);
    }
  }
  bool narrowed = c->length == LEN_BIG_L && LONG_DOUBLE_IS_DOUBLE;
  if (!narrowed) {
    memcpy(w, c->length_text, c->nlength);
    w += c->nlength;
  }
  *w++ = c->conv;
  *w = '\0';
  *out = f;
  return true;
}

#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wformat-nonliteral"
static bool render_one(struct buffer *b, const struct plan *plan,
                       const struct conversion *c) {
  union value v = plan->values[c->arg];
  enum kind kind = plan->kinds[c->arg];
  bool nan = false;
  if (kind == KIND_DOUBLE && isnan(v.d)) {
    nan = true;
    v.d = fabs(v.d);
  } else if (kind == KIND_LONG_DOUBLE && isnan(v.ld)) {
    nan = true;
    v.ld = fabsl(v.ld);
  }
  char *f;
  if (!single_format(plan, c, nan, &f))
    return false;
  int n;
#define RENDER(dst, size)                                                      \
  (kind == KIND_INT       ? snprintf(dst, size, f, v.i)                        \
   : kind == KIND_LONG    ? snprintf(dst, size, f, v.l)                        \
   : kind == KIND_POINTER ? snprintf(dst, size, f, v.p)                        \
   : kind == KIND_DOUBLE  ? snprintf(dst, size, f, v.d)                        \
                          : snprintf(dst, size, f, v.ld))
  n = RENDER(NULL, 0);
  bool ok = n >= 0 && reserve(b, (size_t)n);
  if (ok) {
    RENDER(b->data + b->len, (size_t)n + 1);
    b->len += (size_t)n;
  }
#undef RENDER
  free(f);
  return ok;
}
#pragma GCC diagnostic pop

static bool render(const char *format, const struct plan *plan,
                   struct buffer *b) {
  const char *literal = format;
  for (size_t i = 0; i < plan->nconversions; i++) {
    const struct conversion *c = &plan->conversions[i];
    if (!append(b, literal, (size_t)(c->start - literal)))
      return false;
    literal = c->end;
    if (c->arg < 0) {
      if (!append(b, "%", 1))
        return false;
    } else if (c->conv == 'n') {
      store_count(c, plan->values[c->arg].p, b->len);
    } else if (!render_one(b, plan, c)) {
      return false;
    }
  }
  return append(b, literal, strlen(literal));
}

// Whether `format` needs Darwin's own rendering; if so, `out` holds it in
// memory from this host's `malloc` and the result is its length or -1.
static bool darwin_rendering(const char *format, va_list ap, char **out,
                             int *result) {
  struct plan plan;
  if (!has_float_conversion(format) || !parse(format, &plan))
    return false;
  if (!fetch(&plan, ap) || !formats_a_nan(&plan)) {
    plan_free(&plan);
    return false;
  }
  struct buffer b = {0};
  bool ok = render(format, &plan, &b);
  plan_free(&plan);
  if (ok && b.len > INT_MAX) {
    errno = EOVERFLOW;
    ok = false;
  }
  if (!ok) {
    free(b.data);
    *out = NULL;
    *result = -1;
    return true;
  }
  *out = b.data;
  *result = (int)b.len;
  return true;
}

int rosemary_vasprintf(char **out, const char *format, va_list ap) {
  int n;
  if (!darwin_rendering(format, ap, out, &n))
    return vasprintf(out, format, ap);
  return n;
}

int rosemary_vsnprintf(char *buffer, size_t size, const char *format,
                       va_list ap) {
  char *rendered;
  int n;
  if (!darwin_rendering(format, ap, &rendered, &n))
    return vsnprintf(buffer, size, format, ap);
  if (n >= 0 && size > 0) {
    size_t copied = (size_t)n < size - 1 ? (size_t)n : size - 1;
    memcpy(buffer, rendered, copied);
    buffer[copied] = '\0';
  }
  free(rendered);
  return n;
}

int rosemary_vsprintf(char *buffer, const char *format, va_list ap) {
  char *rendered;
  int n;
  if (!darwin_rendering(format, ap, &rendered, &n))
    return vsprintf(buffer, format, ap);
  if (n >= 0)
    memcpy(buffer, rendered, (size_t)n + 1);
  free(rendered);
  return n;
}

int rosemary_vdprintf(int fd, const char *format, va_list ap) {
  char *rendered;
  int n = rosemary_vasprintf(&rendered, format, ap);
  if (n < 0)
    return -1;
  for (size_t done = 0; done < (size_t)n;) {
    ssize_t put = write(fd, rendered + done, (size_t)n - done);
    if (put < 0 && errno == EINTR)
      continue;
    if (put <= 0) {
      free(rendered);
      return -1;
    }
    done += (size_t)put;
  }
  free(rendered);
  return n;
}
