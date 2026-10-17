/* ----------------------------------------------------------------------------
Rosemary's additions to mimalloc, for the guarantees libmalloc makes that
crates/darwin/src/malloc.rs builds on mimalloc. See ROSEMARY.md at the root of
this crate. Included from static.c, so that it sees mimalloc's internals.

1. The fork gate. libmalloc takes every zone's lock before `fork`, so that the
   child of a threaded process can still allocate. mimalloc has no fork
   handlers: a lock another thread holds when `fork` happens stays held in the
   child, where no thread will release it, and the child hangs on its next
   use. Every `mi_lock_t` is taken through this gate (see `atomic.h`). `fork`
   closes it, waits until no thread holds a lock, and opens it again in both
   processes, so the child finds every lock free. A thread that already holds
   a lock is let through for another, as is the forking thread, so that
   neither can be left waiting on the fork.

2. `mi_rosemary_block_state`: what an address is to mimalloc, so that `free`
   can refuse what libmalloc refuses - an address inside a block, and a block
   the page's own bookkeeping shows is already free.
-----------------------------------------------------------------------------*/
#include "mimalloc.h"
#include "mimalloc/internal.h"
#include "mimalloc/prim.h"

#if defined(MI_USE_PTHREADS)

#include <string.h>  // memcpy

static _Atomic(size_t)        mi_fork_inside;   // threads that hold at least one lock
static _Atomic(size_t)        mi_fork_closed;   // nonzero from the prepare handler on
static pthread_mutex_t        mi_fork_mutex = PTHREAD_MUTEX_INITIALIZER;  // one fork at a time
static mi_decl_thread size_t  mi_fork_depth;    // locks this thread holds
static mi_decl_thread bool    mi_fork_counted;  // this thread is in `mi_fork_inside`
static mi_decl_thread bool    mi_fork_forking;  // this thread is forking

// Whether this thread must go through the gate for its next lock.
static inline bool mi_fork_gate_applies(void) {
  return (mi_fork_depth == 0 && !mi_fork_forking);
}

void _mi_fork_gate_enter(void) {
  if (mi_fork_gate_applies()) {
    for (;;) {
      // seq_cst on both sides: either the prepare handler sees this thread
      // inside, or this thread sees the gate closed.
      atomic_fetch_add_explicit(&mi_fork_inside, 1, memory_order_seq_cst);
      if (atomic_load_explicit(&mi_fork_closed, memory_order_seq_cst) == 0) break;
      atomic_fetch_sub_explicit(&mi_fork_inside, 1, memory_order_seq_cst);
      while (atomic_load_explicit(&mi_fork_closed, memory_order_acquire) != 0) {
        _mi_prim_thread_yield();
      }
    }
    mi_fork_counted = true;
  }
  mi_fork_depth++;
}

bool _mi_fork_gate_try_enter(void) {
  if (mi_fork_gate_applies()) {
    atomic_fetch_add_explicit(&mi_fork_inside, 1, memory_order_seq_cst);
    if (atomic_load_explicit(&mi_fork_closed, memory_order_seq_cst) != 0) {
      atomic_fetch_sub_explicit(&mi_fork_inside, 1, memory_order_seq_cst);
      return false;
    }
    mi_fork_counted = true;
  }
  mi_fork_depth++;
  return true;
}

void _mi_fork_gate_leave(void) {
  mi_assert_internal(mi_fork_depth > 0);
  if (--mi_fork_depth == 0 && mi_fork_counted) {
    mi_fork_counted = false;
    atomic_fetch_sub_explicit(&mi_fork_inside, 1, memory_order_seq_cst);
  }
}

static void mi_fork_prepare(void) {
  pthread_mutex_lock(&mi_fork_mutex);
  mi_fork_forking = true;
  atomic_store_explicit(&mi_fork_closed, 1, memory_order_seq_cst);
  // A thread forking from inside mimalloc (a visitor that forks) holds a lock
  // of its own, which it cannot wait for.
  const size_t own = (mi_fork_counted ? 1 : 0);
  while (atomic_load_explicit(&mi_fork_inside, memory_order_seq_cst) > own) {
    _mi_prim_thread_yield();
  }
}

static void mi_fork_parent(void) {
  mi_fork_forking = false;
  atomic_store_explicit(&mi_fork_closed, 0, memory_order_seq_cst);
  pthread_mutex_unlock(&mi_fork_mutex);
}

static void mi_fork_child(void) {
  // This thread is the only one left, and no other held a lock at the fork.
  // One that had just counted itself in, to find the gate closed, never
  // reached its lock.
  atomic_store_explicit(&mi_fork_inside, (mi_fork_counted ? 1 : 0), memory_order_relaxed);
  atomic_store_explicit(&mi_fork_closed, 0, memory_order_relaxed);
  mi_fork_forking = false;
  const pthread_mutex_t fresh = PTHREAD_MUTEX_INITIALIZER;
  memcpy(&mi_fork_mutex, &fresh, sizeof(fresh));
}

// Called once, from `mi_process_init`. Registered this early, the handlers run
// after every other prepare handler and before every other child handler, so
// those may still allocate.
void _mi_fork_gate_init(void) {
  pthread_atfork(&mi_fork_prepare, &mi_fork_parent, &mi_fork_child);
}

#endif  // MI_USE_PTHREADS


// What `p` is to mimalloc. `free` hands mimalloc only `MI_ROSEMARY_LIVE`.
typedef enum mi_rosemary_block_state_e {
  MI_ROSEMARY_FOREIGN  = 0,  // in no mimalloc page
  MI_ROSEMARY_LIVE     = 1,  // the start of a block that may be live; in a page of aligned
                             // blocks, any address in one, as the aligned address is not kept
  MI_ROSEMARY_INTERIOR = 2,  // in a page, but not where a block starts
  MI_ROSEMARY_FREED    = 3,  // a block freed last on its page, on this thread or another
  MI_ROSEMARY_UNUSED   = 4,  // a block the owning thread's page shows is not in use
} mi_rosemary_block_state_t;

// What `mi_rosemary_block_state` finds beside the state, from the same page
// lookup, so that `free` need not look the page up again.
typedef struct mi_rosemary_block_info_s {
  mi_heap_t* heap;    // the heap whose page `p` is in; NULL when it is in none
  size_t     usable;  // `mi_usable_size(p)` for `MI_ROSEMARY_LIVE`; 0 otherwise
} mi_rosemary_block_info_t;

// Only what is cheap: a block deeper in a free list is `MI_ROSEMARY_LIVE`. A
// page's bookkeeping beyond its constant fields and its atomic thread-free
// list is read only by the thread that owns the page.
mi_decl_export int mi_rosemary_block_state(const void* p, mi_rosemary_block_info_t* info) mi_attr_noexcept {
  info->heap = NULL;
  info->usable = 0;
  mi_page_t* const page = _mi_safe_ptr_page(p);
  if (page == NULL) return MI_ROSEMARY_FOREIGN;
  info->heap = mi_page_heap(page);
  const uint8_t* const start = mi_page_start(page);
  const size_t bsize = mi_page_block_size(page);
  if ((const uint8_t*)p < start) return MI_ROSEMARY_INTERIOR;   // the page's own header
  const size_t diff = (size_t)((const uint8_t*)p - start);
  if (diff >= bsize * page->reserved) return MI_ROSEMARY_INTERIOR;  // past the last block
  const mi_block_t* const block = _mi_page_ptr_unalign(page, p);
  if ((const void*)block != p && !mi_page_has_interior_pointers(page)) {
    return MI_ROSEMARY_INTERIOR;
  }
  if (mi_page_thread_free(page) == block) return MI_ROSEMARY_FREED;
  if (mi_page_thread_id(page) == _mi_prim_thread_id()) {
    if (page->local_free == block) return MI_ROSEMARY_FREED;
    if (page->free == block || page->used == 0 || diff >= bsize * page->capacity) {
      return MI_ROSEMARY_UNUSED;
    }
  }
  info->usable = _mi_usable_size(p, page);
  return MI_ROSEMARY_LIVE;
}
