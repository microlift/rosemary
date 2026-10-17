# libmimalloc-sys 0.1.49, patched

This is `libmimalloc-sys` 0.1.49 from crates.io (mimalloc 3.3.2), used in place
of the published crate through `[patch.crates-io]` in the workspace manifest.
Only the v3 sources it builds are kept; `c_src/mimalloc/v2` and v3's
documentation, tests and build files are left out, so the `v2` feature does
not build.

`crates/darwin/src/malloc.rs` builds libmalloc's zones on mimalloc heaps, and
two of libmalloc's guarantees need mimalloc's internals:

- **The fork gate** (`src/rosemary.c`, `include/mimalloc/atomic.h`,
  `src/init.c`). libmalloc's fork handlers take every zone's lock before
  `fork`, so the child of a threaded process can still allocate. mimalloc has
  none: a lock another thread holds at the fork (a thread starting or exiting
  holds the main heap's `theaps_lock`, for one) stays held in the child, which
  hangs on its next use. Every `mi_lock_t` is now taken through a gate that
  `fork` closes, waiting until no thread holds a lock, and opens again in both
  processes. A thread already holding a lock passes for another, and so does
  the forking thread. mimalloc registers the handlers itself in
  `mi_process_init`, so they run after every other prepare handler and before
  every other child handler. The gate costs an atomic add and subtract on the
  first lock a thread takes; mimalloc takes locks only on slow paths.
- **`mi_rosemary_block_state(p, &info)`** (`src/rosemary.c`): whether `p` is
  where a block starts, and whether the page's bookkeeping shows that block
  free. This lets `free` refuse an address inside a block, and a block freed
  twice when it is the last block freed on its page, as libmalloc does, rather
  than handing either to mimalloc's free lists. It also gives the page's heap
  and the block's usable size, from the same page lookup, which `free` would
  otherwise look up twice more; with it, `free` costs what it did before.
- **`build.rs`** names `c_src/mimalloc/v3` with `rerun-if-changed`. The `cc`
  crate prints `rerun-if-env-changed`, after which cargo watches only what a
  build script names, so an edit here would otherwise not be rebuilt.

`rosemary.patch` is the whole change against the published crate. To move to
a newer `libmimalloc-sys`, copy its `Cargo.toml`, `build.rs`, `LICENSE.txt`,
`src/` and `c_src/mimalloc/v3/{include,src,LICENSE}` here, then
`git apply rosemary.patch` from this directory, and run
`cargo test -p darwin --test libmalloc_conformance` (its
`fork_while_allocating` check hangs without the gate).
