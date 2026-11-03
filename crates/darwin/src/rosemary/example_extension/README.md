# Rosemary example extension

This is a basic extension built using the Rosemary Darwin extension API.

Build it with `make` (needs `clang` and `ld64.lld`), then run a guest with it inserted:

```sh
ROSEMARY_EXPOSE_LIBSYSTEM_EXTENSIONS=1 \
ROSEMARY_DYLD_INSERT_LIBRARIES=./crates/darwin/src/rosemary/example_extension/example_extension.dylib \
./target/release/rosemary /bin/echo test
```

It works under both the `native` and `emulated` backends.
