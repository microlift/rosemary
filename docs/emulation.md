# Rosemary as an emulator

Rosemary contains an abstraction known as the "execution context". Execution contexts are simple at heart: They
determine how to call function pointers, and how to set up the runtime for the execution context's given
architecture.

In the code an execution context is the `Execution` trait in `crates/rosemary/src/exec/`, and there are two
implementations of it:

| backend | type | runs | how a function is called |
| --- | --- | --- | --- |
| `native` | `exec::Native` (`exec/native.rs`) | a slice of the host's own architecture | a plain host call |
| `emulated` | `vm::Vm` (`vm.rs`) | an x86-64 slice | recompiled through Microlift, one region at a time |

Everything above the trait - the Mach-O loader, dyld, the Objective-C runtime, the frameworks, the Darwin
shims - is shared. A backend only answers the questions that genuinely differ between running instructions
directly and recompiling them.

## Choosing a backend

`Engine::select` looks at the file's slices (a thin file counts as one slice) and prefers `native`:

1. `native`, if some slice is the host's CPU type and `native::runnable` accepts it.
2. `emulated`, if some slice is x86-64.
3. Otherwise the program is refused.

A matching instruction set is not enough for `native`. The code is still Darwin's, and it reads its
thread-specific data straight out of the thread pointer register, so `darwin::tls::can_enter_darwin` has to
agree that the host can give it that array. On aarch64 Linux it cannot - Darwin arm64 code reads `TPIDRRO_EL0`,
which Linux does not let userspace write - so a universal binary there takes its x86-64 slice instead.

An `arm64e` slice is accepted only when pointer authentication is inert in this process (probed, not
assumed), since the loader stores pointers unsigned.

`rosemary run --native` and `--emulated` force a backend. `Engine::refusal` turns a forced backend that cannot
take the file into a sentence naming what the binary does contain.

## The `Execution` trait

| method | purpose |
| --- | --- |
| `engine` | which backend this is |
| `report` | the loader's `LinkReport`: images, segments, initializers, entry point |
| `main_args` | `(argc, argv, envp, apple)` for initializers and `main` |
| `call` | call a function in the program with up to six integer arguments |
| `prepare` | backend set-up after the fault handler and before the first initializer |
| `enter_main` | enter the root image's entry point |
| `realize_loaded_objc` | register the Objective-C metadata of a `dlopen`ed image |
| `run_loaded_load_methods` | run that image's `+load` methods |

The trait deliberately carries no register context. Registers, exits and thunks are emulation vocabulary,
and the native backend has none of them; anything that needs them talks to `Vm` directly.

## Lifecycle

`main.rs`'s `run` drives every program the same way:

1. Choose the engine.
2. Load and link: `Native::load`, or `Program::load_executable` followed by `Vm::with_policy`.
3. Leak the backend - the program's mappings must outlive `main` - and `exec::publish` it.
4. `startup::run`:
   1. install the fault handler and note the image map,
   2. install the Steam bridge and (off macOS) the comm page,
   3. `prepare()`,
   4. clear `errno`, because the runtime's own start-up leaves junk in it,
   5. fill each image's `__DATA,__dyld` section,
   6. run every initializer through `call`, in reverse load order,
   7. `enter_main()`,
   8. run the main thread's thread-local terminators.
5. `std::process::exit` with `main`'s status. Returning from Rust's `main` would tear down this thread's TLS
   before the guest's `atexit` handlers run, and those may need to re-enter the JIT.

### Why there is a global backend

`exec::active()` returns the published backend. Code reached as an ordinary import - most importantly
`dlopen` - has no `Execution` reference of its own, yet must run the loaded image's initializers and `+load`
methods through whichever backend launched the process. dyld also asks it which architecture to parse a loaded
image as, and the TLV code which `tlv_get_addr` entry descriptors should point at.

## The native backend

`Native` maps a host-architecture image with `MmapSpace` and calls into it directly.

* **Linking.** The loader is configured with `HOST_SPEC`. Symbols resolve through the same providers the
  emulated path uses (runtime shims, `darwin`, the Objective-C dispatch layer, frameworks, dyld, Steam,
  OpenGL, OpenAL), and fall through to the host's real dylibs unless strict imports are on. Host symbols that
  diverge from Darwin semantics, host frameworks, and foreign Objective-C classes are denied so that Rosemary's
  own implementation is bound instead.
* **No trampolines.** `trampoline::disarm()` runs first: a trampoline is x86-64 code for the lifter to
  intercept, and under `native` a `dlsym` result must be the host function itself.
* **Raw syscalls.** The guest's instructions run on the host CPU, so a raw `syscall`/`svc` would reach the host
  kernel with Darwin's numbering. `darwin::syscall::setup_syscall_handler` interposes them where the host
  allows it.
* **`call`.** Transmutes the address to `extern "C" fn(u64 x 6) -> u64` and calls it with all six argument
  registers set. Under both SysV and AAPCS64 a non-variadic callee ignores registers it did not declare, so
  no signature is needed. For the duration of the call `darwin::tls::DarwinThread::enter()` installs the
  Darwin TSD array as the thread pointer, since `pthread_self`, `errno` and the stack canary are direct loads
  off it. On macOS this compiles away.
* **Entry.** An `LC_MAIN` image is entered by `call` on the host stack. An `LC_UNIXTHREAD` image expects
  `argc` at the stack pointer rather than a return address, so `enter_unix_thread` moves the stack pointer to
  the `GuestStack`'s kernel frame and jumps; it never returns, as the entry point ends with `exit`.
* **Objective-C.** With the builtin frameworks, `prepare` registers the program's classes in Rosemary's runtime,
  then `rewrite_refs` points the guest's class references at the registered classes - nothing intercepts the
  sends a native image makes, so they must already hold real receivers. With the host frameworks on macOS the
  images are handed to the host libobjc via `map_images`.
* **Instruction cache.** Freshly mapped executable segments are flushed before anything runs.

## The emulated backend

`Vm` holds the linked `Program`, the `CodeCache` of translations, the thunk table, a `GuestView` over guest
memory, and the realized Objective-C runtime. All of it is shared between guest threads; per-thread state
(stack, call depth, code window and link tables, the published register context, pending errors) lives in
`thread::ThreadState`. The thread that builds the `Vm` becomes the guest's main thread, since it owns the stack
`argv` was laid out on.

### Entering guest code

`Vm::call` places up to six integer arguments in `rdi, rsi, rdx, rcx, r8, r9`; `call_with_stack` additionally
copies stack-passed arguments (such as SysV `MEMORY`-class aggregates). Either way the return address pushed
is the **return stub** (`retstub.rs`): a never-executable page of `int3`. When the guest returns there and
executes the breakpoint, the run loop knows the call it was servicing has finished and returns `rax`.
`jump_to` enters without a return address, which is how an `LC_UNIXTHREAD` entry point is reached.

`Vm::enter` is the single place every entry passes through, so it sets up everything that is a fact about the
current thread:

* `gs_base` - the Darwin TSD array (`darwin::tls::tsd_base`), which x86-64 Darwin code reads through `%gs`;
* the inline callbacks (`bridge::callbacks`) lifted code uses for `syscall` and for instructions Microlift
  hands to `interp.rs`;
* this thread's block-link table and return ring;
* the nesting depth, capped at `MAX_DEPTH` (64) host-to-guest re-entries;
* a fresh entry identity for `setjmp`/`longjmp`;
* `thunk::with_current`, which makes this `Vm` reachable from a thunk called inside a host function.

### The run loop

`run_loop` repeats until the call that started it returns:

1. Retire translations whose bytes the guest has overwritten (`smc.rs`), and replay retirements into this
   thread's tables.
2. Look the PC up in the thread's code window, then the shared `CodeCache`. A PC that is an import site
   (the guest *jumped* to an import) is serviced here and the guest resumes at the popped return address.
3. Lift the region if nothing has it, and enter it. Linked exits stay inside generated code; only a miss comes
   back to the loop.
4. `step` services the exit:
   * `Breakpoint` at the return stub - the call is done. Any other `int3` is the guest's own and cannot be
     delivered yet.
   * `UnknownJump` - control left the region; an indirect jump to one of Rosemary's own thunks is redirected to
     the guest function behind it.
   * `ImportedFunctionCall` - marshal the arguments by the ABI signature in `abi/table.rs` and call the host.
   * `IndirectCall` - decide whether the target is guest code, an import, or a thunk (`dispatch.rs` caches the
     verdict per call site).
   * `Cpuid` - answered by `darwin::cpu::cpuid`.
   * anything else is a `VmError::Trap`.
5. Raise any error a nested callback parked on this thread.

### Guest to host: imports and trampolines

Rosemary does not recompile system libraries. Each import resolves to a host function, and a call to it is an
exit that marshals the guest's SysV arguments into the host ABI. Under the default import policy
`Program::install_trampolines` points import slots at small x86-64 trampolines (`trampoline.rs`), which let
Microlift recognise the call at lift time and service it through a fast bridge without a full exit.
`ROSEMARY_TRAMPOLINES=0` or the `AlwaysSlow` policy turns them off.

### Host to guest: thunks

A guest function pointer that leaves for host code - a `qsort` comparator, a `pthread_create` start routine, an
Objective-C `IMP` - is replaced by a **thunk** (`thunk.rs`): host-ABI code compiled with Cranelift that converts
the host's arguments into a `RegisterContext` and calls back into `Vm::call_with`. Typed thunks are built from an
Objective-C type encoding, so floating-point arguments and aggregates are placed correctly. A thread the guest
spawned reaches the `Vm` through `thunk::publish` and gets its `ThreadState` on its first entry.

A failure inside a thunk cannot unwind through the host frame that called it, so it is parked on the thread
(`Vm::set_pending`) and raised as soon as control is back in the run loop.

### `prepare` and Objective-C

`prepare` warms the self-modifying-code guard and hooks cross-task `vm_protect`/`vm_write` into it, starts the
macOS-only profiler and overlay, registers the program's Objective-C classes, and runs `+load` before any C
constructor, as dyld does. Unlike `native` there is no `rewrite_refs` pass: every message send from an emulated
image is intercepted, and `objc::dispatch` translates the receiver on the way through.

## Adding to the trait

A method belongs on `Execution` only when both backends must answer it and their answers differ in kind -
`realize_loaded_objc` is there because `dlopen` used to assume a `Vm` and silently skipped native images. Anything
expressed in registers, exits or translations belongs on `Vm`.
