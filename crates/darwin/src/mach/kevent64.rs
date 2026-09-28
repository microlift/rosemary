#![cfg(not(target_os = "macos"))]
//! `kevent64`, and the one filter of it that a Mach port's queue can answer.
//!
//! ## What the guest actually asks for
//!
//! `base::WaitableEvent::WaitManyImpl`
//! (`base/synchronization/waitable_event_apple.cc:130`, read at
//! `refs/branch-heads/6723`, the branch the shipped framework was built from -
//! the `130.0.6723.91` tag has no `mach_port_peek` in that file at all) is one
//! `kevent64` call and nothing else:
//!
//! ```c
//!   std::vector<kevent64_s> events(count);
//!   for (size_t i = 0; i < count; ++i) {
//!     EV_SET64(&events[i], raw_waitables[i]->receive_right_->Name(),
//!              EVFILT_MACHPORT, EV_ADD, 0, 0, i, 0, 0);
//!   }
//!   std::vector<kevent64_s> out_events(count);
//!   ScopedFD wait_many(kqueue());
//!   PCHECK(wait_many.is_valid()) << "kqueue";
//!   int rv = HANDLE_EINTR(kevent64(wait_many.get(), events.data(), count_int,
//!                                  out_events.data(), count_int, /*flags=*/0,
//!                                  /*timeout=*/nullptr));
//!   PCHECK(rv > 0) << "kevent64";
//! ```
//!
//! and the machine code in `Chromium Framework+[0x5d9a7f0, 0x5d9ac3c)` - the
//! whole function is one prologue, the loop that builds the change list, the
//! `kqueue()` at `+0x5d9a990` and the call at `+0x5d9a9db` - agrees with every
//! word of it:
//!
//! ```text
//!  5d9a8b0  mov    rdi,QWORD PTR [r13+rcx*8+0x0]   ; raw_waitables[i]
//!  5d9a8b5  mov    rdi,QWORD PTR [rdi+0x8]          ; -> receive_right_
//!  5d9a8b9  mov    edi,DWORD PTR [rdi+0x10]         ; -> Name(), a 32-bit word
//!  5d9a8bc  mov    QWORD PTR [rsi],rdi              ; ident  = the port name
//!  5d9a8bf  mov    QWORD PTR [rsi+0x8],0x1fff8      ; filter|flags|fflags
//!  5d9a8c7  mov    QWORD PTR [rsi+0x10],0x0        ; data   = 0
//!  5d9a8cf  mov    QWORD PTR [rsi+0x18],rcx        ; udata  = i
//!  5d9a8d3  movaps XMMWORD PTR [rsi+0x20],xmm0     ; ext[0], ext[1] = 0
//! ```
//!
//! The `0x1fff8` is three fields in one eight-byte store, which is the whole of
//! `<sys/event.h>`'s packing read back: `0xfff8` as an `int16_t` is
//! `EVFILT_MACHPORT` (`-8`), `0x0001` as a `uint16_t` is `EV_ADD`, and the
//! remaining four bytes are `fflags == 0`. `0x20` is 32 bytes past the `udata`,
//! so the element is 48 bytes, which is `sizeof(struct kevent64_s)` and not the
//! 32-byte `struct kevent` the sibling `kevent` takes.
//!
//! **The six numbers that matter, therefore, are: `EVFILT_MACHPORT`, `EV_ADD`,
//! `fflags == 0`, `data == 0`, `udata == the caller's index`, and a null
//! timeout.** Everything below is in service of that one shape.
//!
//! ## What a delivered event says
//!
//! `<sys/event.h>` documents the answer, and it is not a count:
//!
//! > If no message receipt options were provided in the fflags field on setup,
//! > no message is received by this call. Instead, on output, the data field
//! > simply contains the name of the actual port detected with a message
//! > waiting.
//!
//! So `data` is a `mach_port_t`, and for a port set it is the *member* that has
//! the message rather than the set. Chromium reads only `udata` and takes the
//! lowest index it was given, which is what `WaitManyImpl` is documented to
//! return.
//!
//! ## The other `EVFILT_MACHPORT` form: receive during delivery
//!
//! There is exactly one other `kevent64` call site in the framework that
//! registers a Mach port, and it is not `WaitManyImpl`. It is
//! `MessagePumpKqueue::MessagePumpKqueue` (`base/message_loop/
//! message_pump_kqueue.cc:146-155`), the pump every `SequenceManager` thread
//! runs on, and the whole registration is these nine instructions at
//! `Chromium Framework+0x5da59a8`:
//!
//! ```text
//!  5da59a8  mov    eax,DWORD PTR [rbx+0x10]        ; wakeup_.get(), a 32-bit name
//!  5da59ab  mov    QWORD PTR [rbp-0x178],rax        ; ident
//!  5da59b2  movabs rax,0x20001fff8
//!  5da59bc  mov    QWORD PTR [rbp-0x170],rax        ; filter|flags|fflags
//!  5da59c3  lea    rax,[rbx+0x18]                   ; &wakeup_buffer_
//!  5da59c7  mov    QWORD PTR [rbp-0x158],rax        ; ext[0]
//!  5da59ce  mov    QWORD PTR [rbp-0x150],0x20       ; ext[1] = 32
//!  5da59e0  mov    edi,DWORD PTR [rbx+0xf0]         ; kqueue_.get()
//  5da59fe  call   0xcca1728                         ; _kevent64
//! ```
//!
//! `0x0000_0002_0001_fff8` read the way the structure declares is
//! `filter == EVFILT_MACHPORT`, `flags == EV_ADD`, and
//! `fflags == 0x2 == MACH_RCV_MSG`, with `ext[0]` naming a 32-byte buffer and
//! `ext[1]` its length. `MessagePumpKqueue` allocates a receive right for its
//! own wakeup port and registers it with the message to be **received into that
//! buffer as part of delivering the event**, so that `DoInternalWork`'s wait
//! returns having already consumed the message `ScheduleWork()` sent.
//!
//! `<sys/event.h>` specifies that form exactly:
//!
//! > The fflags field can optionally contain the MACH_RCV_MSG, MACH_RCV_LARGE,
//! > and related trailer receive options as defined in `<mach/message.h>`.
//! > The presence of these flags directs the kevent64() call to attempt to
//! > receive the message during kevent delivery, rather than just indicate that
//! > a message exists. On setup, The ext[0] field contains the receive buffer
//! > pointer and ext[1] contains the receive buffer length. Upon event
//! > delivery, the actual received message size is returned in ext[1]. As with
//! > mach_msg(), the buffer must be large enough to receive the message and the
//! > requested (or default) message trailers. In addition, the fflags field
//! > contains the return code normally returned by mach_msg().
//!
//! and there is nothing in it about port sets. An earlier version of this file
//! claimed otherwise - that "the SDK documents that form for port sets only" -
//! and that claim is **false**: it is not in the header at all, and it is
//! contradicted by the guest, which uses the form on a plain receive right in
//! shipping code that runs on macOS every day. The refusal it justified was a
//! refusal of the right thing for a reason that does not exist, and it aborted
//! the browser (`PCHECK(rv == 0) << "kevent64"` at line 155 is a hard
//! `CHECK_EQ`, and a hard abort is a refusal the guest cannot tolerate).
//!
//! So it is served, and the reason it can be served *truthfully* is the whole
//! subject of [`Registration::received`]: a Mach port's queue in this runtime
//! is a bound `AF_UNIX`/`SOCK_DGRAM` socket, so "a message is waiting" is
//! `POLLIN` on a real descriptor and "receive the message" is
//! [`super::msg::mach_msg`], which takes the datagram off the queue and writes
//! a real message into the caller's buffer or refuses and says so. The event is
//! reported only after that receive has run, and the receive *consumes* the
//! message, so the event goes quiet until a new one arrives: the level/edge
//! question `EV_CLEAR` exists for is answered by the queue rather than by a bit
//! in a knote.
//!
//! ## `EVFILT_PROC`, on a real `pidfd`
//!
//! `EVFILT_PROC` is "has this process exited", and this host has an answer for
//! it that is not a poll loop: a **`pidfd`**, which the kernel makes readable
//! exactly when the process it names becomes a zombie.
//!
//! ### Why a `pidfd` and not a look-alike
//!
//! A `pidfd` **does not reap**. Nothing this runtime does with one can consume
//! the exit status, so it cannot steal the `waitpid` the guest's own children
//! are collected with. The alternatives all do steal it or degrade it: a
//! `signalfd` for `SIGCHLD` is delivered to *this* process and says nothing
//! about *which* child, an `inotify` watch on `/proc` is a Linux-shaped thing
//! with no macOS counterpart, and a polling thread is a different primitive
//! wearing the same name - and it busy-waits.
//!
//! ### The registration, from the guest's own bytes
//!
//! The framework has exactly **three** `EVFILT_PROC` registrations, and all
//! three are the same eight instructions. Two of them:
//!
//! ```text
//!  5afcf35  call   0xcca0e1c                ; _kqueue
//!  5afcf45  mov    QWORD PTR [rbp-0x80],rbx  ; ident = a pid
//!  5afcf49  movabs rax,0x800000000001fffb    ; EVFILT_PROC | EV_ADD, fflags NOTE_EXIT
//!  5afcf53  mov    QWORD PTR [rbp-0x78],rax
//!  5afcf5a  movups XMMWORD PTR [rbp-0x70],x0 ; data = 0, udata = 0
//!  5afcf72  call   0xcca0e16                ; _kevent
//! ```
//!
//! and the third is `base::Process::WaitForExitWithTimeout` at
//! `Chromium Framework+0x5d8cb10`, byte for byte the same store. `0xfffb` read
//! as an `int16_t` is `EVFILT_PROC`, `0x0001` is `EV_ADD` **and nothing else**,
//! and `0x80000000` is `NOTE_EXIT` - the top bit of `fflags`, and nothing to do
//! with `EV_EOF`, which is a bit of `flags` and is `0x8000`. The two are
//! different fields with different widths and confusing them is the easiest way
//! to get this filter wrong.
//!
//! **And it is `_kevent`, not `_kevent64`.** The stub at `0xcca0e16` is
//! `_kevent`; `_kevent64` is the stub at `0xcca1728`. The 32-byte
//! `struct kevent` these three sites fill in is served by
//! [`crate::kqueue::kevent`], which is where the filter used to die: a silent
//! `EINVAL` from a `matches!` that allowed `EVFILT_READ`, `EVFILT_WRITE` and
//! `EVFILT_USER` and nothing else, with no message and therefore no name for the
//! capability that was missing. That gap is closed, and it is closed *here*
//! rather than by duplicating the filter: [`crate::kqueue::apply_proc`] calls
//! [`proc_change`] for the decision, [`pidfd_open`] for the descriptor and holds
//! a [`Proc`] in its own `HashMap<(ident, filter)>`, next to the readiness
//! registrations it already keeps. This file still does not see the delivery -
//! `kqueue.rs` has its own wait - but every decision about what the filter
//! *means* is made in one place, from the XNU citations above.
//!
//! ### The three sites, and which of them can run at all
//!
//! A count, from the eight-byte pattern rather than from a reading of the code:
//! the registration word `0x8000_0000_0001_fffb` occurs **exactly three times**
//! in the whole 214 MB image, at `0x5afcf4b`, `0x5d8cb12` and `0x5daa1b4`.
//! Those are:
//!
//! | address | function | source | reaches this filter? |
//! |---|---|---|---|
//! | `0x5afcf35` | `RelauncherSynchronizeWithParent` | `chrome/browser/mac/relauncher.mm` | yes, on macOS, in the relauncher process |
//! | `0x5d8cb10` | `base::WaitForSingleNonChildProcess` | `base/process/process_posix.cc` | yes, from `Process::WaitForExitWithTimeout` |
//! | `0x5daa16f` | `base::WaitForChildToDie` | `base/process/kill_mac.cc` | **no** - see below |
//!
//! The third one is the interesting one, because it is the only one that *looks*
//! like it could kill something. It is `base::WaitForChildToDie(pid_t child, int
//! timeout)`, reached in the source only from `base::EnsureProcessTerminated`
//! (`kill_mac.cc`), and it registers the filter, waits with a deadline, and on
//! anything other than a clean `NOTE_EXIT` for that pid does `kill(child,
//! SIGKILL)` followed by a **blocking** `waitpid(child, NULL, 0)`. Every one of
//! those three - the `kill`, the blocking reap, the `DPLOG`s that would have
//! named the file - is in the disassembly at `Chromium Framework+0x5daa381` and
//! `+0x5daa3a0`, and the `DLOG`/`DPLOG`s are absent because **Chromium's
//! release build compiles them out**. That is why the file could not be
//! attributed for so long: there is no `__FILE__` string in the object at all.
//! (The `__FILE__` bracket for `0x5daa16f` is 0x535 after a
//! `thermal_state_observer_mac.mm` marker and 0x1129 before a `launch_mac.cc`
//! one, i.e. it spans two translation units and attributes nothing.)
//!
//! **It cannot be the fault of a `--headless` run, and that is a measurement
//! rather than an argument.** `0x5daa16f` has **no referrer anywhere in the
//! image**: no `call`/`jmp rel32` to it in any section (`e8`/`e9` scan over
//! `__TEXT,__text`, positive control: the same scan finds 12 call sites of the
//! `_kevent` stub and 1555 referrers of the helper it calls at `+0x5daa3c2`), no
//! rip-relative `lea`/`mov` of its address, and no absolute 64-bit occurrence of
//! the value in any segment. It is not reachable from the shipped browser, so no
//! amount of `EVFILT_PROC` correctness affects it. It is implemented anyway,
//! because a filter that is refused in one `kevent` and answered in the other
//! would be a wrong answer for a caller that did exist.
//!
//! ### What the guest checks, and what it does not
//!
//! All three callers read `filter`, `fflags` and `ident` off the delivered
//! event. **None of them reads `flags`, and none of them reads `data`.** That
//! is worth stating because it is what makes the delivered `flags` - where
//! `EV_EOF` and `EV_CLEAR` live, and where this file could most easily be
//! wrong - a field that decides nothing for this guest. It is still reported as
//! XNU sets it, because a caller that *did* read it would be owed the truth:
//! see [`PROC_DELIVERED_FLAGS`] for the four bits and the chain that puts them
//! there.
//!
//! ### What is refused, and why that is an answer
//!
//! Every other filter, and **every refusal names the filter, the flags, the
//! `ident`, and how many times that filter has been refused in this process.**
//! That last part is not decoration. The single `Once` this replaced printed one
//! paragraph the first time *any* filter was refused and was then silent about
//! every other one, so a run that refused `EVFILT_VNODE` twenty times and
//! `EVFILT_AIO` once named only the first - which is the worst shape a refusal
//! can take here, because the guest gets `EINVAL` as an `EV_ERROR` event's
//! `data` and gets nothing else either way.
//!
//! `EVFILT_READ` and `EVFILT_WRITE` are the case that needs the most care,
//! because **`EVFILT_READ` on a Mach port and `EVFILT_READ` on a descriptor are
//! different questions** and the message this file used to print answered the
//! wrong one. It said, of every `ident` it was handed, "a Mach port is not a
//! descriptor" - a claim about the *filter*, asserted about the *number*, and
//! the number is what decides whether the refusal is right. So
//! [`describe_ident`] looks: a live receive right in this task's port space, a
//! port set, an open descriptor on this host (`kqueue()` hands back the
//! readiness descriptor itself, so a guest `ident` can be a real fd), or none of
//! those, and the refusal says which test it used.
//!
//! ### The whole-bundle census: which filters the guest passes, and which it waits on
//!
//! The immediate is in the instruction stream, so the census is read out of it
//! rather than out of a header. Method: every executable section of all
//! fourteen Mach-O images in the bundle was scanned, **at every byte offset**,
//! for `e8 <rel32>` landing on `_kevent` (`0xcca0e16`), `_kevent64` (`0xcca1728`)
//! or `_kqueue` (`0xcca0e1c`) - resolved through the **indirect symbol table
//! within `__TEXT,__stubs`'s declared size only**, because past that size lies
//! `__TEXT,__stub_helper`, which is not a 6-byte-stride table and is where a
//! name comes to have two addresses. Positive control: 12 `_kevent` call sites
//! in the Framework, the number this file's own note above already claimed.
//!
//! | image | `_kevent` | `_kevent64` | `_kqueue` |
//! |---|--:|--:|--:|
//! | Chromium Framework | 12 | 9 | 7 |
//! | chrome_crashpad_handler | 2 | 0 | 1 |
//! | the other twelve images | 0 | 0 | 0 |
//!
//! Every one of those 21 `kevent`/`kevent64` call sites was then disassembled and
//! its change list read instruction by instruction. The result, with **"wait"**
//! meaning the call passed a non-zero `nevents` and so could block:
//!
//! | call site | change list | wait? | what it is |
//! |---|---|:--:|---|
//! | `+0x5d9a9db` | `EVFILT_MACHPORT\|EV_ADD` | **yes** | `WaitableEvent::WaitManyImpl` |
//! | `+0x5da59fe` | `EVFILT_MACHPORT\|EV_ADD\|MACH_RCV_MSG` + buffer | no | `MessagePumpKqueue`'s wakeup port |
//! | `+0x5da6e3e` | `EVFILT_MACHPORT\|EV_ADD` | no | the pump's second receive right |
//! | `+0x5da6427` | none | **yes** | `DoInternalWork`, the wait every pump turn ends in |
//! | `+0x5da62d7` | `EVFILT_TIMER\|EV_ADD\|EV_ONESHOT`, `fflags 0x2` | no | the pump's deadline timer |
//! | `+0x5da61cf` | `EVFILT_TIMER\|EV_DELETE` | no | deregistering that timer |
//! | `+0x5da571e` | `EVFILT_MACHPORT\|EV_DELETE` | no | deregistering a receive right |
//! | `+0x5da4eec` | `EVFILT_READ\|EV_DELETE` + `EVFILT_WRITE\|EV_DELETE` | no | `MessagePumpKqueue`'s deregistration |
//! | **`+0x5da7c0d`** | **`EVFILT_READ\|EV_ADD\|EV_ONESHOT`** + `EVFILT_WRITE\|EV_ADD\|EV_ONESHOT` | **no** | **the real refusals - see below** |
//! | `+0x5afcf72` | `EVFILT_PROC\|EV_ADD`, `NOTE_EXIT` | no | `RelauncherSynchronizeWithParent` |
//! | `+0x5afd036` | none | **yes** | the relauncher waiting on its own kqueue |
//! | `+0x5d8cb53` | `EVFILT_PROC\|EV_ADD`, `NOTE_EXIT` | no | `base::WaitForSingleNonChildProcess` |
//! | `+0x5d8ce63` | none | **yes** | `Process::WaitForExitWithTimeout` |
//! | `+0x5daa1e2` | `EVFILT_PROC\|EV_ADD`, `NOTE_EXIT` | no | `base::WaitForChildToDie` |
//! | `+0x5daa304` | none | **yes** | that function's wait, with a deadline |
//! | `+0x100076801` | `EVFILT_MACHPORT\|EV_ADD\|EV_CLEAR` + `EVFILT_WRITE\|EV_ADD\|EV_CLEAR` | no | crashpad `child_port_handshake.cc:173` |
//! | `+0x100076a28` | none | **yes** | crashpad's wait on the same queue |
//!
//! ### The `EVFILT_READ` refusals are a diagnostic, and here is the whole argument
//!
//! **The immediate is not at the call site, and finding it is the whole
//! difficulty.** `+0x5da7c0d` is the only one of the nine `kevent64` sites whose
//! change list carries no filter immediate anywhere near it: the list is a
//! `std::vector` of 48-byte records, built by a loop, and the call only walks
//! it. The record is filled field by field, and the filter and the flags are
//! two *separate* stores - which is why a scan for the 8-byte word
//! `0x00000000_0011ffff` finds **nothing** in the whole 214 MB image, and why
//! the first pass of this census, which looked for that word, named the wrong
//! site. The stores are:
//!
//! ```text
//!  5da7a4a  movw  $0xffff,0x8(%r14)   ; filter = EVFILT_READ
//!  5da7a51  mov   %bx,0xa(%r14)        ; flags  = EV_ADD|EV_ONESHOT, built at +0x5da79c4
//!  5da7a56  movq  $0x0,0xc(%r14)       ; fflags = 0
//!  5da7b58  movw  $0xfffe,0x8(%r14)   ; filter = EVFILT_WRITE
//!  5da7b5f  mov   %bx,0xa(%r14)        ; the same flags
//!  5da7c0d  call  0xcca1728            ; _kevent64, eventlist NULL, nevents 0
//! ```
//!
//! The flags word is `(%r14b ^ 1) << 4 | 1`, i.e. `EV_ONESHOT` when a bit is
//! clear and `EV_ONESHOT|EV_DISPATCH` when it is set, always with `EV_ADD`.
//! **So the guest registers a one-shot read and a one-shot write on a
//! descriptor, on a kqueue of its own, and then waits on the queue descriptor
//! some other way.** Three facts follow, and together they settle it.
//!
//! **1. `nevents == 0` at `+0x5da7c0d`, so this call cannot block.** Look at
//! the argument setup at `+0x5da7c05`: `xor %ecx,%ecx` for `eventlist` and
//! `xor %r8d,%r8d` for `nevents`. There is nowhere for an event to go. Nothing
//! here can hang a thread, so "the refusals are a hang" is not available, and
//! the census line says so on every shape for exactly that reason.
//!
//! **2. The wait, if there is one, is on the queue *descriptor*, and it is a
//! `kevent` on the same queue - not on these filters.** The site's own
//! `MOVUPS`-cleared record and the `leaq 0x38(%r13)` at `+0x5da7a99` are the
//! pump's own bookkeeping. What the runtime census settles is that a `--headless
//! --dump-dom about:blank` run reaches this shape **exactly once**
//! (`ROSETTA3_TRACE_KEVENT=1`, run 218), submits it, and moves on.
//!
//! **3. The caller tolerates the failure.** `+0x5da4ef4`, in the sibling
//! deregistration path, retries only on `EINTR` and otherwise carries on; and
//! for the registration at `+0x5da7c0d` the shape is submitted by
//! `MessagePumpKqueue`'s watchable-object bookkeeping, which has already stored
//! the descriptor it is about to wait on.
//!
//! The cost, then, is what the brief describes for a *held* filter: **none**,
//! because the queue is the thing being waited on and the queue is real. A
//! refused one-shot registration means the guest is not told *which* readiness
//! fired - and on this host the `EV_ONESHOT` read was going to be a readiness
//! this runtime could not have produced truthfully anyway, since a Mach port
//! that `kevent64` was asked about here is a descriptor, and a descriptor's
//! readiness is a `poll` on it, which [`crate::kqueue`] does.
//!
//! ### How many, and where the "26" came from
//!
//! **The 26 in the brief is not what a run does.** With the count in the refusal
//! message, every run measured here refuses `EVFILT_READ` **once**: runs 215,
//! 218, 221, 222, 223, 224, 225 and 226 all reach `refusal 1 of this filter`
//! and no more. The census agrees - the `EVFILT_READ|EV_ADD|EV_ONESHOT` shape
//! is a single shape, and its count line reads 1 in each.
//!
//! So 26 was most likely the number of *runs* that had shown the line, or the
//! number of images a tool scanned, rather than a count within one run. The
//! honest statement is the one above: **one registration per run, refused once,
//! on a call that cannot block.** If 26 ever does appear it will now be
//! *stated* in the message rather than inferred, which is the only reason to
//! prefer the counted refusal over a `Once`.
//!
//! **What it does cost is the log**, which is why the refusal now says what the
//! `ident` is and how many times it has happened. The `ident` in a real run is
//! `0x23`, and the refusal says of it: *not a Mach port in this task's space,
//! but an open descriptor on this host* - which is the finding, because the
//! message this replaced asserted the opposite of that about every `ident` it
//! was handed.
//!
//! ### The census is not complete, and the way it is not is worth recording
//!
//! The static half found the filter **at the wrong site on the first pass**,
//! and the reason is general enough to be worth writing down: **a change list
//! built field-by-field has no filter immediate to find.** `+0x5da7c0d` writes
//! `filter` and `flags` as two `movw`/`mov` stores eight bytes apart, so the
//! 8-byte pattern `0x00000000_0011ffff` that encodes all three fields at once -
//! the shape the three `EVFILT_PROC` sites use - **occurs zero times in the
//! image**. A scan that looks for the combined word reports "no `EVFILT_READ`
//! registration anywhere", and the first pass of this census did exactly that
//! and named `+0x5da4eec` instead.
//!
//! What caught it was the *runtime* half: the refusal now prints the flags it
//! was actually handed, and they were `EV_ADD|EV_ONESHOT` where the static
//! census said `EV_DELETE`. A number that contradicts the census is the whole
//! point of printing it.
//!
//! The rule that follows: **a static census of change lists is a lower bound,
//! and only a lower bound.** What makes this one exhaustive is not the
//! immediates - it is that all 21 call sites were disassembled and every one of
//! their argument setups read, including the sites whose list is built
//! elsewhere. Positive control for the call-site scan: 12 `_kevent` sites,
//! 9 `_kevent64`, 7 `_kqueue` in the Framework, 2 and 1 in
//! `chrome_crashpad_handler`, and the three `EVFILT_PROC` 8-byte words at
//! exactly `0x5afcf4b`, `0x5d8cb12`, `0x5daa1b4` - the last of which is the
//! positive control for the immediate scan too, and the reason the trap below
//! was caught rather than believed.
//!
//! ### The trap: `macho.py`'s `stub_at` gives one name two addresses
//!
//! Walking `__TEXT,__stubs` past its **declared size** lands in
//! `__TEXT,__stub_helper`, which is not a 6-byte-stride table, and an index
//! computed there resolves to a plausible-looking wrong name. Resolving
//! `_kevent` that way yields **two** addresses, `0xcca0e16` and `0xcca3318`,
//! and the second is in `__stub_helper`. Within the declared size there is
//! **one**: `0xcca0e16`, whose bytes are `ff 25 5c a1 9d 01`, a real
//! `jmp qword ptr [rip+…]` thunk. Every address in the table above is resolved
//! within the declared size, and every call site found is a `call rel32` landing
//! on one of them.
//!
//! (The separate claim about `ff 25 <disp32>` thunks - 1580 thunks, 1580
//! distinct names, none with more than one - is about a different thing and is
//! not re-litigated here.)
//!
//! ### `EVFILT_VNODE` is waited on, and is the one refusal that could be a wall
//!
//! Two `FilePathWatcherKQueue` classes register `EVFILT_VNODE` and then block on
//! it - `base::FilePathWatcherKQueue` and
//! `content::file_system_access::FilePathWatcherKQueue`, at `+0x5d9e968`,
//! `+0x5d9f56e`, `+0x3a9ab38` and `+0x3a9b73e`, every one of them
//! `kevent(kq, changes, n, out, n, NULL)`: a **null timeout and room for
//! events**, which is a wait. The registration word is identical in all four and
//! decodes as the tag's source spells it,
//! `EV_SET(&event, fd, EVFILT_VNODE, EV_ADD|EV_CLEAR|EV_RECEIPT, <all seven notes>, 0, data)`:
//!
//! ```text
//!  3a9a9b8  movabs rax,0x6f0061fffc
//!  3a9a9c2  mov    rax,0x8(%r12)         ; filter 0xfffc, flags 0x0061, fflags 0x0000006f
//! ```
//!
//! with `0x6f` being `NOTE_DELETE|NOTE_WRITE|NOTE_EXTEND|NOTE_ATTRIB|NOTE_LINK|
//! NOTE_RENAME|NOTE_REVOKE`. (The `__FILE__` strings that attribute those two
//! functions are in the image: `0xd809888` is
//! `content/browser/file_system_access/file_path_watcher/file_path_watcher_kqueue.cc`
//! and `0xd8f03b8` is `base/files/file_path_watcher_kqueue.cc`, with
//! `0xd8097f0` naming the `fsevents` sibling as a control.)
//!
//! **So the sentence this file used to close with - that `EVFILT_PROC` was the
//! only filter any of these callers waited on - was true only of the three
//! `EVFILT_PROC` callers, and false of the framework.** It is corrected. What
//! ### It is reached: `EVFILT_VNODE` costs this run four refused registrations
//!
//! **A first measurement said otherwise and was wrong, and the way it was wrong
//! is the useful part.** `ROSETTA3_TRACE_KEVENT=1` over runs 215, 216 and 218
//! showed **no `EVFILT_VNODE` shape at all**, and I wrote that the watcher is
//! not constructed in a `--headless` run. Runs 219 and 220, same command, same
//! binary, no code change in between: **four `EVFILT_VNODE` refusals each.** The
//! census is not lying in either direction - it prints every shape a run reaches
//! - so the difference is in **the run**, not the tool.
//!
//! What differs is the browser's own start-up order, which is what the
//! nondeterministic wall in the brief is about: a run that gets further reaches
//! the watcher, a run that aborts earlier does not. `EVFILT_VNODE` is therefore
//! reached *sometimes*, and which sometimes is not something this file can say.
//!
//! The census lines for it, from run 223, are the two shapes there are:
//!
//! | shape | count | verdict |
//! |---|--:|---|
//! | `EVFILT_VNODE\|EV_ADD\|EV_CLEAR\|EV_RECEIPT`, `fflags 0x6f`, 1 change, 1 event slot | 1 | **can block** |
//! | the same, 3 changes, 3 event slots | 2 | **can block** |
//!
//! **So the honest verdict on `EVFILT_VNODE` is: refused, reached, blocking, and
//! not yet the wall.** The three `fflags 0x6f` registrations are refused with
//! `EINVAL`; `kevent` then answers `-1`; and at every one of the four call sites
//! the guest retries only on `EINTR` and otherwise falls through to a path that
//! proceeds - so a refused `EVFILT_VNODE` is **not** a hang, and the run carries
//! on. What it costs is a file watcher that never fires, which in a
//! `--dump-dom about:blank` run means nothing observable, and which in a
//! browsing run would be a silently dead `FileSystemAccess` watch.
//!
//! **It is implemented now, and the paragraph that used to say otherwise is
//! this one's error record.** It read: *"Implementing it is the honest next step
//! and is not attempted here. The truthful answer is `inotify` on the watched
//! path's parent directory plus a self-write on the file, and this host's
//! `inotify` cannot distinguish `NOTE_WRITE` on a directory from `IN_ATTRIB`,
//! nor report `NOTE_RENAME` against a name."* Both halves of that are wrong, and
//! wrongly in the direction of refusing more than the evidence requires:
//!
//! * **A watch does not need the parent directory.** `inotify_add_watch` on the
//!   object itself is enough, and this filter's `ident` is a descriptor the
//!   caller holds, so the path comes from `/proc/self/fd/<ident>` - the object's
//!   own path, not its parent's. The parent-plus-self-write scheme is what you
//!   would do for a *name*; the `ident` here is a descriptor, which is the thing
//!   XNU attaches the knote to.
//! * **`inotify` separates both of those, and it was measured rather than
//!   assumed.** A watched directory's child create arrives as `IN_CREATE` **with
//!   a name**, and a watched directory's *own* metadata change arrives as
//!   `IN_ATTRIB` **without one**; and a rename of the watched object arrives as
//!   `IN_MOVE_SELF`, which is a statement about the object rather than a name, so
//!   `NOTE_RENAME` needs no name to be reported against. What `inotify` genuinely
//!   cannot do is fold `NOTE_EXTEND` out of `IN_MODIFY`, or `NOTE_LINK` out of
//!   `IN_ATTRIB` - and the answer to that is not a refusal, it is a narrowing:
//!   see [`VNODE_UNREPORTABLE`], and [`VNODE_DROPPED_BY_THE_GUEST`] for the
//!   measurement that this guest never reads those two bits back.
//!
//! So the filter is answered on a real `inotify` watch, in [`crate::kqueue`], for
//! the five notes a measured `inotify` event can carry, and the remaining notes
//! are **dropped from the interest and named** rather than mapped to the nearest
//! event. A note this implementation cannot observe never appears in a delivered
//! `fflags`.
//!
//! Also refused, by name and with a message: a `fflags` receive option beyond
//! [`MACH_RCV_MSG`] ([`refused_receive_option`]), a `MACH_RCV_MSG` registration
//! with no receive buffer or a zero-length one ([`refused_receive_buffer`]), a
//! `MACH_RCV_MSG` registration of a *port set* rather than a single receive
//! right ([`refused_receive_on_a_set`]), a `flags` argument beyond
//! [`KEVENT_FLAG_IMMEDIATE`] ([`refused_flags`]), a registration whose
//! `ident` is not a receive right or a port set in this task's space, and -
//! for `EVFILT_PROC` - a `fflags` beyond [`NOTE_EXIT`]
//! ([`refused_proc_note`]), which is the whole of what a `pidfd` can say.
//!
//! ## The `flags` argument, and the second `CHECK` in the same file
//!
//! `kevent64`'s sixth argument is a **value**, and the framework's two calls
//! that pass one differ in exactly one bit. `MessagePumpKqueue::DoInternalWork`
//! (`message_pump_kqueue.cc:465-473`, the wait every pump turn ends in):
//!
//! ```text
//!  5da63dc  mov    r12b,r13b          ; flags = immediate
//!  5da6412  mov    edi,DWORD PTR [rbx+0xf0]   ; kqueue_.get()
//  5da6418  mov    QWORD PTR [rsp],0x0        ; timeout = nullptr
//!  5da6420  xor    esi,esi             ; changelist = nullptr
//!  5da6422  xor    edx,edx             ; nchanges  = 0
//!  5da6424  mov    r9d,r12d            ; flags
//!  5da6427  call   0xcca1728           ; _kevent64
//! ```
//!
//! so `flags` is `KEVENT_FLAG_IMMEDIATE` (`0x1`) when the pump wants to be told
//! what is already pending and `0` when it is willing to block. XNU's public
//! `bsd/sys/event.h` is one line about it:
//!
//! ```c
//! #define KEVENT_FLAG_IMMEDIATE  0x000001  /* immediate timeout */
//! ```
//!
//! "Immediate timeout" is a zero timeout, and that is all it is served as -
//! including the case that matters, where the caller's `timeout` pointer is
//! **null** and would otherwise mean "block forever". `PCHECK(rv > 0)` at line
//! 473 is the same hard `CHECK_EQ` as the one at line 155, so the difference
//! between serving this bit and refusing it is between a pump that runs and a
//! browser that aborts. The other public bit, `KEVENT_FLAG_ERROR_EVENTS`,
//! names a delivery path this filter does not have and is refused.
//!
//! The one thing that cannot be got right here and is not attempted: a
//! registration whose port is destroyed while a `kevent64` is waiting on it.
//! Nothing in this namespace announces a port's death, so rather than invent a
//! wakeup the registration is reported once as `EV_ERROR` with `data ==
//! EBADF` and then dropped. Reporting a port as *readable* when it is not is
//! the failure this file is written to avoid: Chromium would spin on a
//! `mach_msg` that can never return a message.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_int, c_void};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use super::space::{self, Name};

/// `struct kevent64_s` from `<sys/event.h>`, in the field order it declares.
///
/// **Not packed, and not the 32-byte `struct kevent`.** The two differ in their
/// `data` and `udata` widths, in the trailing `ext[2]`, and therefore in their
/// size: 32 bytes against 48. A stride read off the wrong one puts every event
/// after the first at the wrong offset, which is silent corruption rather than
/// a failure, and the framework's own stride is 48 (`add rsi,0x30` in the loop
/// that zeroes the change list, `lea r15,[rax+rax*2]` for `count * 48`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Kevent64 {
    /// A `mach_port_t` for `EVFILT_MACHPORT`, a descriptor for the filters that
    /// name one. Stored as 64 bits because the structure says so; the guest
    /// writes a 32-bit name into the low half and leaves the high half zero.
    pub ident: u64,
    pub filter: i16,
    pub flags: u16,
    pub fflags: u32,
    pub data: i64,
    pub udata: u64,
    /// Filter-specific extensions. `EV_SET64`'s last two arguments, which the
    /// receive-during-delivery form of `EVFILT_MACHPORT` uses as a receive
    /// buffer and its length; zero for everything this serves.
    pub ext: [u64; 2],
}

const _: () = {
    assert!(size_of::<Kevent64>() == 48);
    assert!(std::mem::offset_of!(Kevent64, filter) == 8);
    assert!(std::mem::offset_of!(Kevent64, fflags) == 12);
    assert!(std::mem::offset_of!(Kevent64, data) == 16);
    assert!(std::mem::offset_of!(Kevent64, udata) == 24);
    assert!(std::mem::offset_of!(Kevent64, ext) == 32);
};

const EVFILT_READ: i16 = -1;
const EVFILT_WRITE: i16 = -2;
const EVFILT_AIO: i16 = -3;
/// `EVFILT_VNODE`, from the same `<sys/event.h>` as the rest of this block:
/// `EVFILT_VNODE = -4`.
///
/// **`pub` because [`crate::kqueue`] serves it**, for the same reason
/// [`EVFILT_PROC`] is: the framework's two `FilePathWatcherKQueue` classes reach
/// this filter through `_kevent` - the 32-bit `struct kevent` at the stub
/// `0xcca0e16` - and never through this file's `kevent64`, so the constant is what
/// `crate::kqueue::kevent` switches on to find its way here. The spelling is
/// this module's so that the number exists exactly once in the crate.
pub const EVFILT_VNODE: i16 = -4;
/// `EVFILT_PROC`, from the same `<sys/event.h>` as the rest of this block:
/// `EVFILT_PROC = -5`.
///
/// **`pub` because [`crate::kqueue`] serves it.** All three of the framework's
/// `EVFILT_PROC` registrations go through `_kevent` - the stub at `0xcca0e16` -
/// and never reach this file's [`kevent64`], so this constant is what
/// [`crate::kqueue::kevent`] switches on to find its way here. The spelling is
/// this module's so that the number exists exactly once in the crate.
pub const EVFILT_PROC: i16 = -5;
const EVFILT_SIGNAL: i16 = -6;
const EVFILT_TIMER: i16 = -7;
const EVFILT_MACHPORT: i16 = -8;
const EVFILT_FS: i16 = -9;
const EVFILT_USER: i16 = -10;
const EVFILT_VM: i16 = -12;
const EVFILT_EXCEPT: i16 = -15;

const EV_ADD: u16 = 0x0001;
const EV_DELETE: u16 = 0x0002;
const EV_ENABLE: u16 = 0x0004;
const EV_DISABLE: u16 = 0x0008;
const EV_ONESHOT: u16 = 0x0010;
const EV_CLEAR: u16 = 0x0020;
const EV_RECEIPT: u16 = 0x0040;
const EV_DISPATCH: u16 = 0x0080;
const EV_ERROR: u16 = 0x4000;
/// `EV_EOF`, from the same header: `#define EV_EOF 0x8000 /* EOF detected */`.
///
/// **A bit of `flags`, and nothing to do with `NOTE_EXIT`.** `NOTE_EXIT` is the
/// top bit of the 32-bit `fflags` (`0x8000_0000`); `EV_EOF` is bit 15 of the
/// 16-bit `flags`. The framework's own `EVFILT_PROC` registration stores
/// `0x8000_0000_0001_fffb`, and reading the `0x80000000` as `EV_EOF` is the
/// mistake this constant exists to prevent. It is reported in a delivered
/// `EVFILT_PROC` event, which is where XNU puts it - see [`Proc::fired`].
const EV_EOF: u16 = 0x8000;

/// `__NR_pidfd_open` on x86-64, from this host's own kernel UAPI header
/// `/usr/include/x86_64-linux-gnu/asm/unistd_64.h:351`:
///
/// ```c
/// #define __NR_pidfd_open 434
/// ```
///
/// **Not an invented constant, and the tree is not asked to take this on
/// trust.** Three independent things agree on `434`: that header; the `libc`
/// crate this workspace already depends on, whose
/// `src/unix/linux_like/linux/gnu/b64/powerpc64/mod.rs` (and `sparc64`,
/// `loongarch64`, and the 32-bit `x86`) all declare
/// `SYS_pidfd_open: c_long = 434`; and the call itself, which
/// `pidfd_open_of_this_process_works` runs on every `cargo test`. The `libc`
/// crate has **no** `SYS_pidfd_open` for `x86_64` - an upstream gap, the only
/// architecture of the eleven it lists that lacks it - which is why the number
/// is written here rather than referenced.
///
/// If a kernel ever lacks the syscall, [`pidfd_open`] gets `ENOSYS` and the
/// filter refuses by name ([`refused_proc_unavailable`]) rather than answering
/// something else.
// The kernel's own spelling, kept so the name can be searched for in
// `/usr/include/x86_64-linux-gnu/asm/unistd_64.h` and found.
#[allow(non_upper_case_globals)]
const SYS_pidfd_open: libc::c_long = 434;

/// `KEVENT_FLAG_IMMEDIATE`, from `<sys/event.h>`, whose own comment is
/// "immediate timeout".
///
/// **Not a guess about the name.** XNU's public `bsd/sys/event.h` declares
/// `#define KEVENT_FLAG_IMMEDIATE  0x000001  /* immediate timeout */` and
/// `#define KEVENT_FLAG_ERROR_EVENTS  0x000002  /* output events only include
/// change errors */` in the same header the `EVFILT_*` values above come from.
///
/// It is also what the guest passes, read out of the binary:
/// `MessagePumpKqueue::DoInternalWork` sets
/// `unsigned int flags = immediate ? KEVENT_FLAG_IMMEDIATE : 0;` and the
/// machine code at `Chromium Framework+0x5da63dc` (`mov r12b,r13b`, feeding
/// `mov r9d,r12d` at `+0x5da6424`) is exactly that, with `immediate` true on
/// the first pass of `Run` and on any pass after `DoWork` reported more to do.
const KEVENT_FLAG_IMMEDIATE: u32 = 0x0000_0001;

/// The `fflags` of an `EVFILT_TIMER`, from `<sys/event.h>`.
///
/// The first four are cross-checked against the framework's own stores, because
/// XNU's public header and the shipped binary have to agree for any of this to
/// mean anything:
///
/// ```text
///  5da6259  movabs rax,0x20011fff9   ; EVFILT_TIMER|EV_ADD|EV_ONESHOT, fflags 0x2
///  5da62aa  mov    DWORD PTR [rbp-0x64],0x12
///                                   ; ... and 0x12 with leeway
///  5d8cb10  movabs rax,0x800000000001fffb  ; fflags 0x80000000 is NOTE_EXIT, EVFILT_PROC
/// ```
///
/// so `0x2` is `NOTE_USECONDS` and `0x10` is `NOTE_LEEWAY`, which is what
/// `message_pump_kqueue.cc:374,380` writes.
const NOTE_SECONDS: u32 = 0x0000_0001;
const NOTE_USECONDS: u32 = 0x0000_0002;
const NOTE_NSECONDS: u32 = 0x0000_0004;
const NOTE_ABSOLUTE: u32 = 0x0000_0008;
const NOTE_LEEWAY: u32 = 0x0000_0010;
const NOTE_MACH_CONTINUOUS_TIME: u32 = 0x0000_0080;
const NOTE_MACHTIME: u32 = 0x0000_0100;

/// The unit bits a timer's `fflags` chooses between, which XNU switches on as a
/// group: more than one of them set matches no case and is `EINVAL`.
///
/// `NOTE_CRITICAL` (`0x20`) and `NOTE_BACKGROUND` (`0x40`) are deliberately
/// absent and deliberately not constants: they are coalescing hints, and
/// [`announce_ignored_timer_hint`] names them where they are discussed.
const TIMER_UNITS: u32 = NOTE_SECONDS | NOTE_USECONDS | NOTE_NSECONDS | NOTE_MACHTIME;

/// Every `fflags` bit `filt_timervalidate` reads, and therefore every bit that
/// is part of what a timer registration *means*.
///
/// A bit outside this set is not a unit, an epoch or a leeway request, and XNU's
/// own validation does not look at it - see [`announce_ignored_timer_hint`].
const TIMER_MEANINGFUL: u32 = TIMER_UNITS | NOTE_LEEWAY | NOTE_ABSOLUTE | NOTE_MACH_CONTINUOUS_TIME;

/// The action bits an `EVFILT_TIMER` is served for.
///
/// The same set the port filter serves, and for the same reason: a filter that
/// ignored a bit it was given would be a registration doing something other
/// than what it says.
const TIMER_SERVED: u16 = SERVED;

/// The `fflags` of an `EVFILT_PROC`, from the same `<sys/event.h>`, quoted from
/// `bsd/sys/event.h` rather than from memory:
///
/// ```c
/// #define NOTE_EXIT               0x80000000      /* process exited */
/// #define NOTE_FORK               0x40000000      /* process forked */
/// #define NOTE_EXEC               0x20000000      /* process exec'd */
/// #define NOTE_EXITSTATUS         0x04000000      /* exit status to be returned ... */
/// #define NOTE_EXIT_DETAIL        0x02000000      /* provide details on reasons for exit */
/// ```
///
/// `NOTE_EXIT` is also the value in the framework's own registration: the top
/// four bytes of `movabs rax,0x800000000001fffb` at `Chromium Framework+
/// 0x5afcf49`, `+0x5d8cb10` and `+0x5daa1b2` - three sites, one constant.
///
/// **`NOTE_EXITSTATUS` and `NOTE_EXIT_DETAIL` are refused, and the reason is
/// that a `pidfd` cannot produce them.** XNU collects both into the knote's
/// `kn_hook32` by walking the dying `struct proc` (the `wait(2)` status word,
/// and the jetsam / CSKILLED / decrypt-fail reasons); a `pidfd` reports one
/// bit - *it exited* - and reports nothing about how. Reporting an event with
/// `data == 0` under a `NOTE_EXITSTATUS` registration would be a fabricated
/// exit status, so [`refused_proc_note`] refuses it by name instead.
const NOTE_EXIT: u32 = 0x8000_0000;
const NOTE_EXITSTATUS: u32 = 0x0400_0000;
const NOTE_EXIT_DETAIL: u32 = 0x0200_0000;

/// The three "follow this process across `fork`" `NOTE_` bits, which
/// `filt_procattach` refuses with `ENOTSUP` before it looks at anything else:
///
/// ```c
/// #define NOTE_TRACK      0x00000001   /* follow across forks */
/// #define NOTE_TRACKERR   0x00000002   /* could not track child */
/// #define NOTE_CHILD      0x00000004   /* am a child process */
/// ```
///
/// They are named here so that this file can refuse them with **the same
/// `ENOTSUP` the reference gives** rather than with a refusal of its own
/// invention. That is the difference between matching the kernel and
/// approximating it: the number the guest sees is the number Darwin produces.
const NOTE_TRACK: u32 = 0x0000_0001;
const NOTE_TRACKERR: u32 = 0x0000_0002;
const NOTE_CHILD: u32 = 0x0000_0004;

/// The whole `flags` word XNU delivers for a process that has exited:
/// `EV_ADD | EV_ONESHOT | EV_CLEAR | EV_EOF`.
///
/// **`EV_ADD` is in there, and it is not a guess.** `struct knote` in
/// `bsd/sys/event_private.h` declares `#define kn_flags kn_kevent.kei_flags` -
/// `kn_flags` *is* the `flags` field of the `struct kevent_internal_s` that
/// `kevent_register` fills from the caller's change with
/// `kn->kn_kevent = *(struct kevent_internal_s *)kev;` - and the same header's
/// `knote_fill_kevent_with_sdata` hands that word back to the caller with
/// `*kev = *(struct kevent_qos_s *)&kn->kn_kevent;`. So the caller's own
/// `EV_ADD` is already in the delivered word before any filter touches it, and
/// the filter's three additions land in the same field:
///
/// ```c
/// filt_procattach:  kn->kn_flags |= EV_CLEAR;        /* automatically set */
/// filt_procevent:  kn->kn_flags |= (EV_EOF | EV_ONESHOT);   /* on NOTE_EXIT */
/// ```
///
/// `kevent_legacy_copyout` then copies `.flags = kevp->flags` with no mask, and
/// the only `& ~EV_SYSFLAGS` in `kern_event.c` is on the way in. That is the
/// open question the previous revision of this file left open, answered.
const PROC_DELIVERED_FLAGS: u16 = EV_ADD | EV_ONESHOT | EV_CLEAR | EV_EOF;

/// The action bits an `EVFILT_PROC` is served for.
///
/// The same [`SERVED`] set, and `EV_ONESHOT` and `EV_CLEAR` in it mean nothing
/// a caller can observe wrong: XNU sets `EV_CLEAR` on this filter itself at
/// attach time and `EV_ONESHOT` at `NOTE_EXIT` (see [`Proc::fired`]), so a
/// registration that asked for either gets the same event either way, and one
/// that asked for neither is still one-shot because the process is gone.
const PROC_SERVED: u16 = SERVED;

/// The action bits `EVFILT_MACHPORT` is served for.
///
/// `EV_CLEAR` is in this list and does nothing, and that is a decision rather
/// than an oversight: a port's event is level-triggered by construction, because
/// the message that fired it is still at the head of the queue, so there is no
/// state to clear. A filter that *consumes* its event - a timer, a user event -
/// is what `EV_CLEAR` exists for.
///
/// Everything outside the list is refused rather than ignored: the filter's own
/// `EV_FLAG0` and `EV_FLAG1` mean nothing to a Mach port, and `EV_SYSFLAGS` is
/// reserved by the kernel.
const SERVED: u16 =
    EV_ADD | EV_DELETE | EV_ENABLE | EV_DISABLE | EV_ONESHOT | EV_CLEAR | EV_RECEIPT | EV_DISPATCH;

/// The one message-receipt option `EVFILT_MACHPORT` is served for:
/// `MACH_RCV_MSG` from `<mach/message.h>`, the same value
/// `super::msg`'s own `mach_msg` names `RCV_MSG`.
///
/// `0x2` is not a guess: it is what the guest's one registration stores
/// (`movabs rax,0x20001fff8` at `Chromium Framework+0x5da59b2`, whose top four
/// bytes are `fflags`), and it is `MACH_RCV_MSG` in both
/// `osfmk/mach/message.h` and the Darwin SDK.
const MACH_RCV_MSG: u32 = 0x0000_0002;

/// The receive options `<sys/event.h>` names as meaningful in an
/// `EVFILT_MACHPORT` `fflags` that [`MACH_RCV_MSG`] is not.
///
/// Listed so that a refusal can say which bit it is refusing. `MACH_RCV_LARGE`
/// is the one that matters: with it, a message too big for the caller's buffer
/// is **kept** rather than destroyed, so the port's queue stops being readable
/// while the message is still on it - and this filter has no way to re-report
/// that. See [`refused_receive_option`].
const MACH_RCV_LARGE: u32 = 0x0000_0004;

/// `MACH_RCV_MSG | MACH_RCV_TIMEOUT`, the option word the receive below uses.
///
/// The timeout bit is there so the receive cannot block: the `poll` above has
/// already said a datagram is queued, and the only way it is not there any more
/// is another thread having taken it. Waiting on it would turn a lost race into
/// a hang, and a hang inside a `kevent64` is indistinguishable from a port that
/// will never be written to.
const RCV_NOW: i32 = 0x0000_0102;

/// The receive buffer an `EV_ADD` named, in `ext[0]` and `ext[1]`.
///
/// A `u64` address rather than a pointer, because it came out of the guest's
/// address space and is only turned into a pointer at the point of use, where
/// [`Receive::deliver`] can say so.
#[derive(Clone, Copy)]
struct Receive {
    buffer: u64,
    length: u64,
}

/// What one `EV_ADD` recorded. There is only one of these per port name,
/// because there is only one filter: the key is the name, not the
/// `(ident, filter)` pair a general kqueue would use.
#[derive(Clone, Copy)]
struct Registration {
    ident: Name,
    flags: u16,
    fflags: u32,
    udata: u64,
    enabled: bool,
    /// `Some` when the registration asked for the message to be received as
    /// part of delivering the event, which is `fflags & MACH_RCV_MSG`.
    receive: Option<Receive>,
}

impl Registration {
    /// The event to hand back when this registration fires.
    fn fired(&self, source: Name) -> Kevent64 {
        Kevent64 {
            ident: self.ident as u64,
            filter: EVFILT_MACHPORT,
            // `EV_CLEAR` needs no handling and is not reported: a port's event
            // is level-triggered by construction, because the message that
            // fired it is still at the head of the queue. Only a filter that
            // consumes its event - a timer, a user event - has anything to
            // clear, and this is not one.
            flags: 0,
            // The caller's own `fflags`, and nothing else. The kernel adds a
            // `MACH_NOTE_MSG` bit here for a port that has a message waiting;
            // it is not in `<sys/event.h>`, `<sys/kern_event.h>`,
            // `<mach/port.h>` or `<mach/message.h>`, and a bit this runtime
            // cannot cite is a bit it does not invent.
            fflags: self.fflags,
            // "The name of the actual port detected with a message waiting."
            data: source as i64,
            udata: self.udata,
            ext: [0, 0],
        }
    }

    /// The event for a port whose message was received into the caller's buffer
    /// as part of delivering it.
    ///
    /// This is where "accepted and ignored" would go, and it is the reason this
    /// file is written the way it is:
    ///
    /// * `code` and `size` come from a receive that has already run. There is
    ///   no path to this constructor without [`Receive::deliver`] having called
    ///   [`super::msg::mach_msg`] on the port and taken the datagram off its
    ///   queue. `super::msg` writes the message into the guest's buffer and
    ///   returns `MACH_MSG_SUCCESS` **only** if it did; a buffer that is too
    ///   small, a message whose body does not parse, a port that is not a
    ///   receive right - each returns its own `kern_return_t` and none of them
    ///   is zero, and that value is what lands in `fflags`.
    /// * `<sys/event.h>` is explicit that the `fflags` of the delivered event
    ///   "contains the return code normally returned by mach_msg()", and that
    ///   `ext[1]` comes back as "the actual received message size". Both are
    ///   reported as the header says, so a caller that wants to know whether
    ///   the receive worked can tell.
    /// * And because the receive *consumed* the message, the port is no longer
    ///   readable and the next wait on this queue reports nothing for it until a
    ///   new message arrives. That is the clear, and it is a property of the
    ///   queue rather than a flag this file sets: the event cannot re-fire for
    ///   a message that is not there, and it cannot go quiet for one that is.
    fn received(&self, source: Name, code: i32, size: u64) -> Kevent64 {
        Kevent64 {
            // The `fflags` the caller registered is *not* echoed: the header
            // gives this field a different meaning on the way out, and
            // `MACH_RCV_MSG` is an instruction to receive, not an answer.
            fflags: code as u32,
            data: source as i64,
            // "Upon event delivery, the actual received message size is
            // returned in ext[1]."
            ext: [0, size],
            ..self.fired(source)
        }
    }

    /// The event for a registration whose port is gone.
    ///
    /// `EV_ERROR`'s convention is that `data` carries an `errno`, so a caller
    /// that only reads the flags still learns something went wrong, and the
    /// `udata` is the only thing that says *which* registration.
    fn vanished(&self) -> Kevent64 {
        Kevent64 {
            flags: EV_ERROR,
            data: crate::errno::from_host(libc::EBADF) as i64,
            ..self.fired(self.ident)
        }
    }
}

impl Receive {
    /// Take the message waiting on `source` and write it into the buffer this
    /// registration named, answering the `kern_return_t` `mach_msg` would.
    ///
    /// `MACH_RCV_MSG` with no `MACH_RCV_LARGE`, because that is the option set
    /// [`MACH_RCV_MSG`] admits ([`refused_receive_option`] refuses the rest): a
    /// message that does not fit is destroyed rather than kept, exactly as
    /// `mach_msg` does without that bit, so the port is left quiet and this
    /// filter is not holding a message it cannot report again.
    ///
    /// `msgh_size` is read back out of the buffer rather than returned
    /// separately, so the number reported in `ext[1]` is the size the *guest*
    /// would compute from its own message, not one this file made up.
    ///
    /// # Safety
    ///
    /// `buffer` and `length` came from a guest `EV_ADD`, so they name
    /// `length` writable bytes in this process or they name nothing. A length
    /// this cannot honour is refused before the pointer is used at all, and a
    /// length that overflows is refused here, so the call is only made with a
    /// length that fits in `u32` and a pointer that is either null (and then
    /// nothing is written) or the guest's own.
    unsafe fn deliver(&self, source: Name) -> (i32, u64) {
        if self.length > u32::MAX as u64 {
            return (super::kern::MACH_RCV_TOO_LARGE, 0);
        }
        let buffer = self.buffer as *mut std::ffi::c_void;
        // SAFETY: the caller's own buffer and length, from its own `EV_ADD`.
        // `super::msg::mach_msg`'s contract is stated on its definition: with
        // `MACH_RCV_MSG` it writes `rcv_size` bytes and reads none.
        let code = unsafe {
            super::msg::mach_msg(
                buffer,
                RCV_NOW,
                0,
                self.length as u32,
                source,
                0,
                space::MACH_PORT_NULL,
            )
        };
        if code != super::kern::MACH_MSG_SUCCESS || buffer.is_null() {
            return (code, 0);
        }
        // `msgh_size` is the second `u32` of `mach_msg_header_t`, and it is the
        // size the message itself declares, which is the only "actual received
        // message size" this can cite.
        // SAFETY: a successful receive wrote a whole `mach_msg_header_t`.
        let size = unsafe { buffer.cast::<u32>().add(1).read() } as u64;
        (code, size)
    }
}

/// A `timerfd`'s descriptor, closed when the last holder lets go of it.
///
/// Held behind an `Arc` for the same reason [`space::Receiver`] is: a
/// registration's descriptor outlives the lock that guards the table it is in,
/// and a `close` underneath a `poll` is a use-after-close rather than a
/// failure. The wait below holds an `Arc<Timer>` for exactly as long as the
/// descriptor is in its `pollfd` array.
struct TimerFd(c_int);

impl Drop for TimerFd {
    fn drop(&mut self) {
        // SAFETY: this type's own descriptor, reached only through an `Arc`, so
        // no other holder can be using it.
        unsafe { libc::close(self.0) };
    }
}

/// `int pidfd_open(pid_t pid, unsigned int flags)`, answering with the host's
/// `errno` when the kernel declines.
///
/// Flags are `0`. `PIDFD_NONBLOCK` would make a `poll` return immediately
/// instead of waiting, and `PIDFD_THREAD` adds a thread to the dying process -
/// neither is wanted, and neither is passed.
///
/// The failure that matters is **`ESRCH`**, and it is the reference's own answer
/// rather than a near miss: XNU's `filt_procattach` does
/// `p = proc_find((int)kn->kn_id); if (p == NULL) { knote_set_error(kn,
/// ESRCH); return 0; }`, and both of the framework's `EVFILT_PROC` callers
/// treat `errno == ESRCH` as "the process is not there, so it is dead" -
/// `base::WaitForSingleNonChildProcess` returns `true`, and
/// `mac_relauncher::RelauncherSynchronizeWithParent` returns. So this returns
/// the host's `ESRCH` as Darwin's `ESRCH` and the guest's own dead-process
/// answer runs, unchanged.
/// `pub` because [`crate::kqueue`] opens the `pidfd` for its own registration
/// table. The three framework `EVFILT_PROC` registrations reach
/// [`crate::kqueue::kevent`] and never reach this module's [`kevent64`], so
/// without this the filter that decides what a `pidfd` is for would be
/// unreachable from the one function the guest calls.
pub fn pidfd_open(pid: u32) -> Result<c_int, i32> {
    // SAFETY: a `pid_t` and a flags word. The variadic `syscall` signature is
    // the only way to reach this: the `libc` crate declares `SYS_pidfd_open`
    // for ten architectures and not for `x86_64`, and no wrapper function at
    // all, so [`SYS_pidfd_open`] carries the number and this is the call.
    let fd = unsafe {
        libc::syscall(
            SYS_pidfd_open,
            pid as libc::pid_t,
            0 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(crate::errno::from_host(crate::errno::get()));
    }
    Ok(fd as c_int)
}

/// A `pidfd`'s descriptor, closed when the last holder lets go of it.
///
/// Behind an `Arc` for exactly the reason [`TimerFd`] is: an `EV_DELETE` on
/// another thread drops the queue's own `Arc`, and a `close` underneath a
/// `poll` is a use-after-close rather than a failure. The wait holds an
/// `Arc<Proc>` for as long as the descriptor is in its `pollfd` array, so
/// cancelling a registration while another thread is blocked on this queue
/// closes the `pidfd` only once nobody is polling it.
///
/// A `pidfd` is **not** a child-watch descriptor: it never reaps, so it cannot
/// take the exit status away from the `waitpid` the guest collects its own
/// children with. That property is the whole reason this is a `pidfd` and not
/// one of the look-alikes.
struct PidFd(c_int);

impl Drop for PidFd {
    fn drop(&mut self) {
        // SAFETY: this type's own descriptor, reached only through an `Arc`, so
        // no other holder can be using it.
        unsafe { libc::close(self.0) };
    }
}

/// One `EVFILT_PROC` registration: a `pidfd` and the pid it names.
///
/// `Clone` for the same reason [`Timer`] is cloneable - `EV_ENABLE` and
/// `EV_DISABLE` re-state what the registration already said, and a new `Proc`
/// over the same `Arc<PidFd>` is that without disturbing a descriptor another
/// thread may be polling.
///
/// **`pub` with private fields because [`crate::kqueue`] owns the other
/// registration table.** That module serves the 32-bit `struct kevent` through
/// `_kevent`, which is what every one of the framework's `EVFILT_PROC`
/// registrations actually call, and it has its own `HashMap<(ident, filter)>`
/// and its own arming and cancellation. It cannot use this module's [`Queue`],
/// and this module cannot use its wait; what it can do is hold a `Proc` of its
/// own in *its* table, which is what [`proc_change`] and [`pidfd_open`] are for.
/// The accessors are the whole of that interface: the descriptor to poll, the
/// pid the `ident` named, whether it is armed, the caller's `udata`, and
/// [`Proc::exited`] for what XNU delivers.
#[derive(Clone)]
pub struct Proc {
    fd: Arc<PidFd>,
    /// The pid, as the caller wrote it into `ident`. A `pid_t` is a signed
    /// 32-bit word, so a negative `ident` is a pid of a process group or a
    /// session leader rather than a process, and `pidfd_open` refuses it; the
    /// `u32` here is only ever the value the kernel was given.
    pid: u32,
    /// `EV_DISABLE` was asked for, so this is not polled.
    enabled: bool,
    udata: u64,
}

/// What XNU delivers for a process that has exited, in the four fields that are
/// not the queue's own.
///
/// Split out of [`Proc::fired`] so that a caller with a *different* event
/// structure can be handed the same answer rather than re-deriving it - which is
/// what [`crate::kqueue`] does, since the `struct kevent` it fills in is 32
/// bytes and this module's is 48.
pub struct Exited {
    /// `knote_fill_kevent` copies `kn_kevent` wholesale, and `kn_id` is
    /// `kn_kevent.kei_ident`, so the delivered `ident` is the `ident` the
    /// registration was made with.
    pub ident: u64,
    /// The whole word is [`Proc::exited`]'s own derivation; see there.
    pub flags: u16,
    pub fflags: u32,
    /// `knote_fill_kevent(kn, kev, kn->kn_hook32)`, and `kn_hook32` is zero for
    /// every registration this accepts.
    pub data: i64,
}

impl Proc {
    /// The `pidfd` this registration is watched through, for a caller that
    /// polls it.
    ///
    /// The `ident` of an `EVFILT_PROC` is a **pid, not a descriptor**, so this
    /// is the only way a queue whose table is keyed by `ident` can find out
    /// which descriptor to wait on. Callers that keep descriptor-keyed
    /// registrations in the same table must not treat a `Proc` registration as
    /// one of theirs: a pid and a descriptor share a number space here and must
    /// not be confused.
    pub fn raw_fd(&self) -> c_int {
        self.fd.0
    }

    /// The pid the `ident` named, as a `pid_t` is signed: a negative value names
    /// a process group, which `pidfd_open` refuses with `ESRCH`.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// `EV_DISABLE` was asked for, so this registration is not polled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The caller's `udata`, which comes back in the delivered event.
    pub fn udata(&self) -> u64 {
        self.udata
    }

    /// A registration over a `pidfd` **this module did not open**.
    ///
    /// For [`crate::kqueue`], which opens the descriptor through [`pidfd_open`]
    /// and then has to build the registration around it, because the
    /// registration lives in *that* module's table. The descriptor is adopted
    /// rather than copied: the `PidFd` that owns the `close` is created here, so
    /// the queue's last `Arc` is still what closes it.
    pub fn new(fd: c_int, pid: u32, enabled: bool, udata: u64) -> Proc {
        Proc {
            fd: Arc::new(PidFd(fd)),
            pid,
            enabled,
            udata,
        }
    }

    /// A second registration over the **same** `pidfd`, which is what a
    /// re-`EV_ADD` on the same `(ident, EVFILT_PROC)` and an `EV_ENABLE` or
    /// `EV_DISABLE` of one both are.
    ///
    /// "There may be only one unique kevent per kqueue" means the registration
    /// is replaced, not stacked, so the new one takes the old one's descriptor
    /// and the old `Arc` going away is not a `close` - the new one holds the
    /// same `PidFd`. Doing it the other way round, opening a second `pidfd` and
    /// closing the first, would be a `close` the caller did not ask for and
    /// would race a wait in flight on another thread.
    pub fn sharing_of(other: &Proc, pid: u32, enabled: bool, udata: u64) -> Proc {
        Proc {
            fd: Arc::clone(&other.fd),
            pid,
            enabled,
            udata,
        }
    }

    /// The same registration with its `EV_DISABLE` state changed and nothing
    /// else: the pid and the caller's `udata` are what the registration already
    /// says, and an `EV_ENABLE` change carries neither.
    pub fn rearmed(other: &Proc, enabled: bool) -> Proc {
        Proc {
            fd: Arc::clone(&other.fd),
            pid: other.pid,
            enabled,
            udata: other.udata,
        }
    }

    /// The event for a process that has exited.
    ///
    /// Every field here is read off XNU, and the two that are easy to get wrong
    /// are `flags` and `data`:
    ///
    /// * `flags` is **`EV_ADD | EV_CLEAR | EV_EOF | EV_ONESHOT`**, and all four
    ///   bits are accounted for. The chain is short and it is the same chain for
    ///   all of them, because `struct knote` in `bsd/sys/event_private.h` has
    ///   `#define kn_flags kn_kevent.kei_flags` - `kn_flags` **is** the `flags`
    ///   field of the `struct kevent_internal_s` the knote is built from, and
    ///   `kevent_register` fills that from the caller's change with
    ///   `kn->kn_kevent = *(struct kevent_internal_s *)kev;`. `filt_procattach`
    ///   then does `kn->kn_flags |= EV_CLEAR;` and `filt_procevent` does
    ///   `kn->kn_flags |= (EV_EOF | EV_ONESHOT);`, all three of which are
    ///   therefore bits of the same word, and `knote_fill_kevent_with_sdata`
    ///   hands that word to the caller with `*kev = *(struct kevent_qos_s
    ///   *)&kn->kn_kevent;` before `kevent_legacy_copyout` copies
    ///   `.flags = kevp->flags` through unchanged. So the delivered `flags` is
    ///   **the caller's own `flags` word with the filter's three additions** -
    ///   the framework's registration is `EV_ADD` and nothing else, so
    ///   `0x0001 | 0x0010 | 0x0020 | 0x8000 == 0x8031`. The only `& ~EV_SYSFLAGS`
    ///   in `kern_event.c` is on the way *in* (`kevent_internal`, three times,
    ///   building the internal form from the user's 32- or 64-bit one), so no
    ///   returned-only bit is ever masked away on the way out.
    ///
    ///   **The one thing not claimed is that this list is exhaustive for a
    ///   caller who registered `EV_CLEAR` or `EV_ONESHOT` itself**, which for
    ///   this file's callers is nobody: all three framework registrations store
    ///   `0x8000_0000_0001_fffb`, and `0x0001` as a `uint16_t` is `EV_ADD` and
    ///   nothing else. A registration that asked for either of the other two
    ///   gets the same word, because it is the *same* word.
    ///
    ///   **It decides nothing for this guest**, and that is worth saying
    ///   because it is the field this file could most easily be wrong in: all
    ///   three of the framework's `EVFILT_PROC` callers read `filter`, `fflags`
    ///   and `ident`, and none of them reads `flags`. The field is reported as
    ///   XNU sets it so that a caller which *did* read it would be owed the
    ///   truth, rather than a word that happens to be convenient here.
    /// * `data` is **zero**, and that is a fact about XNU rather than a
    ///   default. `filt_procprocess` fills the event with
    ///   `knote_fill_kevent(kn, kev, kn->kn_hook32)`, and `filt_procevent`
    ///   zeroes `kn_hook32` for every `NOTE_EXIT` and only then adds to it
    ///   `if ((kn->kn_sfflags & NOTE_EXITSTATUS) != 0)` and
    ///   `if ((kn->kn_sfflags & NOTE_EXIT_DETAIL) != 0)`. Both of those are
    ///   refused ([`refused_proc_note`]), so for every registration this file
    ///   accepts, `kn_hook32` is zero and so is `data`. The header's promise
    ///   that "the exit status will be stored in `data`" holds only for a
    ///   `NOTE_EXITSTATUS` registration, and there is nothing here that could
    ///   honestly fill it.
    /// * `fflags` is [`NOTE_EXIT`] and not the caller's word, because
    ///   `filt_procevent` accumulates `kn_fflags |= event` from the notes that
    ///   actually fired rather than echoing `kn_sfflags`.
    /// * The event is **not** echoed for a disabled or deleted registration,
    ///   because there is none: the process is gone, and so is the knote XNU
    ///   would have attached to its `p_klist`.
    pub fn exited(&self) -> Exited {
        Exited {
            ident: self.pid as u64,
            flags: PROC_DELIVERED_FLAGS,
            fflags: NOTE_EXIT,
            data: 0,
        }
    }

    /// The event for a process that has exited, in this module's own structure.
    fn fired(&self) -> Kevent64 {
        let Exited {
            ident,
            flags,
            fflags,
            data,
        } = self.exited();
        Kevent64 {
            ident,
            filter: EVFILT_PROC,
            flags,
            fflags,
            data,
            udata: self.udata,
            ext: [0, 0],
        }
    }
}

/// One `EVFILT_TIMER` registration.
///
/// The deadline is **not** kept as a number this file compares against a clock.
/// It is armed on a real `timerfd`, so the expiry is the host kernel's and the
/// event is the kernel's `POLLIN` on a descriptor that becomes readable when the
/// deadline passes. A deadline computed here and compared in a loop would be a
/// different thing wearing the same name.
///
/// `Clone` because `EV_ENABLE` and `EV_DISABLE` re-state what the registration
/// already said, and a new `Timer` over the same `Arc<TimerFd>` is that without
/// disturbing the descriptor another thread may be polling.
#[derive(Clone)]
struct Timer {
    fd: Arc<TimerFd>,
    /// The armed interval, kept so `EV_ENABLE` can re-arm without the caller
    /// having to say it again, and so a re-arm is the same deadline rather than
    /// a fresh one.
    interval: libc::timespec,
    /// `EV_ONESHOT` was asked for, so the timer is gone once it has fired.
    oneshot: bool,
    /// `EV_DISABLE` was asked for, so the descriptor is armed with no deadline.
    enabled: bool,
    udata: u64,
    fflags: u32,
}

impl Timer {
    /// Arm the descriptor, and answer with the `errno` if the kernel declined.
    ///
    /// `it_interval` is left at zero for a one-shot and set to the full interval
    /// for a repeating one, which is the whole of the difference between "fires
    /// once" and "keeps firing": the two are the same `timerfd` with one field
    /// set.
    ///
    /// A zero `it_value` would be the wrong way to say "already due". A
    /// `timerfd` armed with `it_value == 0` is **disarmed** - the man page is
    /// explicit, and it is the opposite of the meaning - so the smallest armable
    /// deadline is used instead: one nanosecond, which by the next clock tick is
    /// past. XNU reaches the same place from the other end, because
    /// `filt_timerarm` treats an interval of zero as a one-shot: so `data == 0`
    /// and `data < 0` are the same event in the reference implementation.
    fn arm(&self) -> Result<(), i32> {
        let spec = libc::itimerspec {
            it_interval: match self.oneshot {
                true => zero_timespec(),
                false => self.interval,
            },
            it_value: match is_zero(&self.interval) {
                true => libc::timespec { tv_sec: 0, tv_nsec: 1 },
                false => self.interval,
            },
        };
        // SAFETY: a live descriptor from `TimerFd`, and a whole `itimerspec`.
        let rc = unsafe {
            libc::timerfd_settime(
                self.fd.0,
                0,
                &raw const spec,
                std::ptr::null_mut(),
            )
        };
        match rc {
            0 => Ok(()),
            _ => Err(crate::errno::from_host(crate::errno::get())),
        }
    }

    /// The event for a deadline that has passed.
    ///
    /// `fflags` and `udata` are the registration's own, because a timer does
    /// not reinterpret them on the way out; `data` is the time left, which for
    /// an expired timer is zero; and `flags` is zero, because `EV_ONESHOT` and
    /// `EV_CLEAR` describe what the *registration* was and XNU's public
    /// `<sys/event.h>` declares no returned-only flag for a timer (there is no
    /// `EV_TIMER` in it), so there is nothing to report that is not a request
    /// the caller already made.
    fn fired(&self, ident: u64) -> Kevent64 {
        Kevent64 {
            ident,
            filter: EVFILT_TIMER,
            flags: 0,
            fflags: self.fflags,
            data: 0,
            udata: self.udata,
            ext: [0, 0],
        }
    }
}

fn zero_timespec() -> libc::timespec {
    libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    }
}

/// Whether a `timespec` is the zero one. `libc::timespec` is not `PartialEq`,
/// and comparing it field by field is the only way to say "no deadline".
fn is_zero(when: &libc::timespec) -> bool {
    when.tv_sec == 0 && when.tv_nsec == 0
}

#[derive(Default)]
struct Queue {
    registrations: HashMap<Name, Registration>,
    /// Keyed by `ident`, because `(ident, EVFILT_TIMER)` is the timer's identity
    /// and there is only one filter on it - the same rule the port
    /// registrations follow.
    timers: HashMap<u64, Arc<Timer>>,
    /// Keyed by `ident`, for the same reason: `(ident, EVFILT_PROC)` is the
    /// registration's identity and a kqueue may hold only one `kevent` per
    /// `(ident, filter)` pair, so a re-`EV_ADD` of the same pid replaces this
    /// one rather than adding to it. Replacing drops the old `Arc<PidFd>`, and
    /// that is the `close` an `EV_DELETE` is required to cause.
    procs: HashMap<u64, Arc<Proc>>,
}

fn queues() -> &'static Mutex<HashMap<c_int, Arc<Mutex<Queue>>>> {
    static QUEUES: OnceLock<Mutex<HashMap<c_int, Arc<Mutex<Queue>>>>> = OnceLock::new();
    QUEUES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether `queue` is a descriptor [`crate::kqueue::kqueue`] handed out.
///
/// Asked of [`crate::kqueue`] rather than of this module's own table, because
/// that is where the answer is kept: a descriptor that is not a kqueue has to
/// be `EBADF`, and the list of queues is that module's. `kevent` with nothing
/// to apply and nowhere to put an event does nothing at all except answer
/// exactly this question, so it is asked that way rather than by guessing what
/// kind of descriptor `queue` might be.
fn is_queue(queue: c_int) -> bool {
    // SAFETY: `nchanges` is zero and `nevents` is zero, so `kevent` reads
    // neither pointer and writes through none of them.
    unsafe {
        crate::kqueue::kevent(
            queue,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        ) == 0
    }
}

/// The receive rights behind `name`, in the order a caller would look at them:
/// the port itself, or the members of a port set.
///
/// `None` for a name that is neither, which is what makes a registration of one
/// an `EINVAL` rather than a knote that can never fire.
fn ports_behind(name: Name) -> Option<Vec<Name>> {
    if space::receiver(name).is_some() {
        return Some(vec![name]);
    }
    let members = space::members(name)?;
    // A set with no usable member is a set whose event can never fire, which is
    // what Darwin does too, so it registers.
    Some(members)
}

/// The `poll` timeout in milliseconds, or `-1` for "wait forever".
///
/// Rounded **up**: a caller that asked to wait for half a millisecond asked to
/// wait, and a zero here would turn its wait into a poll - and for
/// `WaitManyImpl` a poll that finds nothing is `PCHECK(rv > 0)` failing, which
/// aborts the browser.
fn timeout_ms(timeout: *const libc::timespec) -> Result<c_int, c_int> {
    // SAFETY: null, or the caller's `struct timespec`, which is the same two
    // 64-bit words in this order on both platforms.
    let Some(timeout) = (unsafe { timeout.as_ref() }) else {
        return Ok(-1);
    };
    if timeout.tv_sec < 0 || !(0..1_000_000_000).contains(&timeout.tv_nsec) {
        return Err(crate::errno::from_host(libc::EINVAL));
    }
    let ms = (timeout.tv_sec as i64)
        .saturating_mul(1000)
        .saturating_add((timeout.tv_nsec as i64 + 999_999) / 1_000_000);
    Ok(ms.min(c_int::MAX as i64) as c_int)
}

/// Apply one change, answering with the `errno` it failed with.
///
/// The number is **Darwin's**: it reaches the guest as an `EV_ERROR` event's
/// `data`, which is a value rather than the `errno` slot, so nothing later
/// translates it.
fn apply(state: &mut Queue, change: &Kevent64) -> Result<(), i32> {
    if change.filter == EVFILT_TIMER {
        return apply_timer(state, change);
    }
    if change.filter == EVFILT_PROC {
        return apply_proc(state, change);
    }
    if change.filter != EVFILT_MACHPORT {
        return Err(refused_filter(change));
    }
    if change.flags & !SERVED != 0 {
        return Err(refused_action(change.flags, change.ident));
    }
    let name = name_of(change.ident);
    if change.flags & EV_DELETE != 0 {
        state
            .registrations
            .remove(&name)
            .map(|_| ())
            .ok_or_else(|| {
                // A port set's members are not the registration's key, so this
                // is `ENOENT` for the set's name as well as for a port's.
                announce_missing_registration(name, EV_DELETE);
                crate::errno::from_host(libc::ENOENT)
            })?;
        return Ok(());
    }
    if change.flags & EV_ADD != 0 {
        if ports_behind(name).is_none() {
            return Err(refused_ident(name));
        }
        let receive = match change.fflags {
            // No receipt options: "on output, the data field simply contains the
            // name of the actual port detected with a message waiting."
            0 => None,
            // Any bit that is not `MACH_RCV_MSG` changes what the receive has to
            // do. Checked against *all* of them, not against the one that is
            // served, so `MACH_RCV_MSG | MACH_RCV_LARGE` is refused rather than
            // silently answered as if the second bit were not there.
            f if f & !MACH_RCV_MSG != 0 => {
                return Err(refused_receive_option(change.ident, f));
            }
            // "`fflags & MACH_RCV_MSG` is set, and the `ext[1]` field specifies a
            // zero length, the system call argument specifying an output area
            // (kevent_qos) will be consulted" - and there is no such area
            // behind this call, so the buffer has to be the caller's own.
            _ if change.ext[0] == 0 || change.ext[1] == 0 => {
                return Err(refused_receive_buffer(change.ident, change.ext));
            }
            // A port set's event reports the *member* that has the message, and
            // a receive has to name one port. No registration in the framework
            // does this; saying so is better than guessing which member the
            // caller meant.
            _ if space::receiver(name).is_none() => {
                return Err(refused_receive_on_a_set(change.ident, name));
            }
            _ => Some(Receive {
                buffer: change.ext[0],
                length: change.ext[1],
            }),
        };
        state.registrations.insert(
            name,
            Registration {
                ident: name,
                flags: change.flags,
                fflags: change.fflags,
                udata: change.udata,
                enabled: change.flags & EV_DISABLE == 0,
                receive,
            },
        );
        return Ok(());
    }
    let Some(registration) = state.registrations.get_mut(&name) else {
        announce_missing_registration(name, change.flags);
        return Err(crate::errno::from_host(libc::ENOENT));
    };
    if change.flags & EV_ENABLE != 0 {
        registration.enabled = true;
    }
    if change.flags & EV_DISABLE != 0 {
        registration.enabled = false;
    }
    Ok(())
}

/// The interval a timer's `data` and `fflags` describe, in nanoseconds.
///
/// This follows `filt_timervalidate` in XNU's `kern_event.c` - the function
/// that decides what a timer registration *means* - case for case, including
/// what it refuses:
///
/// * the four unit bits are switched on as a group, and more than one of them
///   set matches no case and is `EINVAL`;
/// * no unit bits at all is **milliseconds**, which is the default and not an
///   error;
/// * `kev->data < 0` is "fire immediately, once", not an error and not a
///   zero-length interval - XNU says so explicitly and explains that the old
///   behaviour was a hot spin;
/// * a `data * multiplier` that overflows is `ERANGE`, which is what XNU
///   returns.
///
/// `NOTE_ABSOLUTE`, `NOTE_MACHTIME` and `NOTE_MACH_CONTINUOUS_TIME` are refused
/// before this is reached ([`refused_timer_epoch`]): all three name a time base
/// this host does not have, and the two Mach ones name Mach time specifically.
fn timer_interval(change: &Kevent64) -> Result<libc::timespec, i32> {
    let multiplier: i128 = match change.fflags & TIMER_UNITS {
        NOTE_SECONDS => 1_000_000_000,
        NOTE_USECONDS => 1_000,
        NOTE_NSECONDS => 1,
        0 => 1_000_000,
        _ => return Err(crate::errno::from_host(libc::EINVAL)),
    };
    // `data < 0`: an already-expired one-shot. Zero rather than negative,
    // because a `timespec` with a negative field is refused by `timerfd_settime`
    // and by `EINVAL` in the reference implementation.
    let nanos: i128 = match change.data {
        d if d < 0 => 0,
        d => (d as i128) * multiplier,
    };
    let ceiling = (i64::MAX as i128) * 1_000_000_000;
    if nanos > ceiling {
        return Err(crate::errno::from_host(libc::ERANGE));
    }
    Ok(libc::timespec {
        tv_sec: (nanos / 1_000_000_000) as libc::time_t,
        tv_nsec: (nanos % 1_000_000_000) as i64,
    })
}

/// What one `EVFILT_PROC` change asks for, decided with no reference to any
/// queue's tables.
///
/// Split out of [`apply_proc`] so that it can be used by a caller that keeps its
/// registrations somewhere else - which is not hypothetical: see [`proc_change`].
pub enum ProcChange {
    /// `EV_ADD`: watch this pid for [`NOTE_EXIT`].
    Watch {
        pid: u32,
        /// `EV_ADD` "implies enable" (`<sys/event.h>`, on `EV_ADD` itself), so
        /// this is false only when `EV_DISABLE` was in the same word.
        enabled: bool,
        udata: u64,
    },
    /// `EV_DELETE`: forget the registration, which is the `close`.
    Delete,
    /// `EV_ENABLE` or `EV_DISABLE` on a registration that is already there.
    Armed { enabled: bool },
    /// The change was rejected. The number is **Darwin's**: it reaches the guest
    /// as an `EV_ERROR` event's `data`, which is a value, so nothing later
    /// translates it.
    Refused(i32),
}

/// Decide what an `EVFILT_PROC` change means, and whether it is allowed.
///
/// **This is the entry point [`crate::kqueue`] uses**, and it now does. That
/// module serves the 32-bit `struct kevent` through `_kevent`, which is what
/// all three of the framework's `EVFILT_PROC` registrations actually call -
/// the stub at `0xcca0e16` is `_kevent`, not `_kevent64` at `0xcca1728` - and
/// it used to refuse the filter in a `matches!` with no message. Its
/// registrations live in a different type from this module's [`Queue`] and its
/// wait is a different mechanism, so it cannot call [`kevent64`] and this file
/// cannot call it back; what it borrows is this decision, and [`pidfd_open`],
/// [`Proc`] and [`Proc::exited`] beside it:
///
/// ```ignore
/// // crates/darwin/src/kqueue.rs::apply_proc, in place of the old filter match:
/// match super::mach::kevent64::proc_change(
///     change.ident as u64, change.flags, change.fflags, change.udata as u64) {
///     super::mach::kevent64::ProcChange::Refused(e) => return Err(e),
///     ...
/// }
/// ```
///
/// and the `Proc` it puts in its own table is waited on through
/// [`Proc::raw_fd`] - the `ident` of this filter is a **pid**, not a descriptor,
/// which is the one thing a descriptor-keyed table has to be told.
///
/// The `fflags` rules are the reference's own, in the reference's own order -
/// `filt_procattach` checks `NOTE_TRACK | NOTE_TRACKERR | NOTE_CHILD` and
/// answers `ENOTSUP` **before it looks at the pid at all** - so a caller that
/// switches on the returned number sees what Darwin produces.
pub fn proc_change(ident: u64, flags: u16, fflags: u32, udata: u64) -> ProcChange {
    if flags & !PROC_SERVED != 0 {
        return ProcChange::Refused(refused_proc_action(flags, ident));
    }
    if fflags & (NOTE_TRACK | NOTE_TRACKERR | NOTE_CHILD) != 0 {
        return ProcChange::Refused(crate::errno::from_host(libc::ENOTSUP));
    }
    // `NOTE_EXIT` and nothing else. A `pidfd` says *it exited* and nothing about
    // how, so every other note is a question this cannot answer - named in
    // `refused_proc_note` rather than ignored.
    if fflags & !NOTE_EXIT != 0 {
        return ProcChange::Refused(refused_proc_note(ident, fflags));
    }
    if flags & EV_DELETE != 0 {
        return ProcChange::Delete;
    }
    if flags & (EV_ENABLE | EV_DISABLE) != 0 && flags & EV_ADD == 0 {
        return ProcChange::Armed {
            enabled: flags & EV_ENABLE != 0,
        };
    }
    ProcChange::Watch {
        pid: pid_of(ident),
        enabled: flags & EV_DISABLE == 0,
        udata,
    }
}

/// Apply one `EVFILT_PROC` change to this queue.
///
/// The decision is [`proc_change`]'s; this only carries it out against
/// [`Queue::procs`], which is what puts a `pidfd` behind it and takes it away
/// again.
fn apply_proc(state: &mut Queue, change: &Kevent64) -> Result<(), i32> {
    let ident = change.ident;
    // **Whether a change creates a registration or changes one is the queue's
    // question, not [`proc_change`]'s** - only the queue has the table - and
    // [`crate::kqueue`] asks it the same way. `kevent_register` answers `ENOENT`
    // when `kq_find_knote_and_kq_lock` finds no knote and `EV_ADD` was not given,
    // so a change that only re-states a registration (`EV_RECEIPT` alone, say)
    // must not be read as a request to watch a process: that would hand the
    // caller a *success* receipt for a watch it never asked to make.
    if change.flags & EV_ADD == 0 && !state.procs.contains_key(&ident) {
        announce_missing_proc(ident);
        return Err(crate::errno::from_host(libc::ENOENT));
    }
    match proc_change(ident, change.flags, change.fflags, change.udata) {
        ProcChange::Refused(errno) => Err(errno),
        ProcChange::Delete => match state.procs.remove(&ident) {
            // The `Arc<PidFd>` this drops is the `close`. A `pidfd` closes when
            // its last reference goes, and the queue's was the last one that
            // could be outstanding: the `poll` below holds its own.
            Some(_) => Ok(()),
            None => {
                announce_missing_proc(ident);
                Err(crate::errno::from_host(libc::ENOENT))
            }
        },
        ProcChange::Armed { enabled } => {
            // Enabling and disabling take a registration out of the wait and put
            // it back, and the `pidfd` is untouched by either: the process has
            // not stopped existing while a knote was disabled, and the
            // descriptor is what the knote *is*.
            let Some(existing) = state.procs.get(&ident) else {
                announce_missing_proc(ident);
                return Err(crate::errno::from_host(libc::ENOENT));
            };
            let mut proc = (**existing).clone();
            proc.enabled = enabled;
            state.procs.insert(ident, Arc::new(proc));
            Ok(())
        }
        ProcChange::Watch { pid, enabled, udata } => {
            let fd = match state.procs.get(&ident) {
                // A re-`EV_ADD` on the same `(ident, EVFILT_PROC)` keeps the
                // descriptor that is already there, which is what "there may
                // only be one unique kevent per kqueue" means: the registration
                // is replaced, not stacked.
                Some(existing) => Arc::clone(&existing.fd),
                None => {
                    let raw = pidfd_open(pid).map_err(|errno| {
                        if errno == crate::errno::from_host(libc::ENOSYS) {
                            refused_proc_unavailable(ident);
                        }
                        errno
                    })?;
                    Arc::new(PidFd(raw))
                }
            };
            state.procs.insert(
                ident,
                Arc::new(Proc {
                    fd,
                    pid,
                    enabled,
                    udata,
                }),
            );
            Ok(())
        }
    }
}

/// The `pid_t` in an `EVFILT_PROC` `ident`.
///
/// A `pid_t` is a signed 32-bit word and `pidfd_open` takes one, so the
/// `ident` is reinterpreted rather than range-checked: `EV_SET64`'s
/// `uintptr_t ident` is how XNU's own `filt_procattach` receives it too -
/// `p = proc_find((int)kn->kn_id)` - and the cast is what the reference does.
/// A negative value names a process group, and `pidfd_open` refuses it with
/// `ESRCH`, which is the reference's answer for an `ident` that is not a
/// process.
fn pid_of(ident: u64) -> u32 {
    ident as u32
}

/// Apply one `EVFILT_TIMER` change.
fn apply_timer(state: &mut Queue, change: &Kevent64) -> Result<(), i32> {
    if change.flags & !TIMER_SERVED != 0 {
        return Err(refused_timer_action(change.flags, change.ident));
    }
    if change.fflags & TIMER_MEANINGFUL != change.fflags {
        announce_ignored_timer_hint(change.ident, change.fflags);
    }
    // All three name a time base this host does not have. `NOTE_MACHTIME`
    // selects `mach_absolute_time`, `NOTE_MACH_CONTINUOUS_TIME` selects
    // `mach_continuous_time`, and `NOTE_ABSOLUTE` - with or without a unit -
    // selects the calendar epoch, which XNU converts through the wall clock
    // *once*, at registration, so that an NTP step cannot move an armed
    // deadline. A `CLOCK_REALTIME` `timerfd` deadline is not that: the step
    // moves it. Refused rather than quietly more fragile than the reference.
    if change.fflags & (NOTE_ABSOLUTE | NOTE_MACHTIME | NOTE_MACH_CONTINUOUS_TIME) != 0 {
        return Err(refused_timer_epoch(change.ident, change.fflags));
    }
    let ident = change.ident;
    if change.flags & EV_DELETE != 0 {
        return match state.timers.remove(&ident) {
            Some(_) => Ok(()),
            None => {
                announce_missing_timer(ident);
                Err(crate::errno::from_host(libc::ENOENT))
            }
        };
    }
    // `EV_ENABLE` and `EV_DISABLE` take a registration out of the set without
    // deleting it, and the interval it was given is kept so `EV_ENABLE` puts
    // back the same deadline rather than one the caller has to state again.
    if change.flags & (EV_ENABLE | EV_DISABLE) != 0 && change.flags & EV_ADD == 0 {
        let Some(existing) = state.timers.get(&ident) else {
            announce_missing_timer(ident);
            return Err(crate::errno::from_host(libc::ENOENT));
        };
        let mut timer = (**existing).clone();
        timer.enabled = change.flags & EV_ENABLE != 0;
        // A disabled timer is a timerfd with no deadline, which is how a
        // `timerfd` is disarmed.
        let spec = libc::itimerspec {
            it_interval: zero_timespec(),
            it_value: match timer.enabled {
                true => timer.interval,
                false => zero_timespec(),
            },
        };
        // SAFETY: a live descriptor from `TimerFd`, and a whole `itimerspec`.
        let rc = unsafe {
            libc::timerfd_settime(timer.fd.0, 0, &raw const spec, std::ptr::null_mut())
        };
        if rc < 0 {
            return Err(crate::errno::from_host(crate::errno::get()));
        }
        state.timers.insert(ident, Arc::new(timer));
        return Ok(());
    }
    let interval = timer_interval(change)?;
    // `filt_timerarm` treats an interval of zero as a one-shot, so `data == 0`
    // and `data < 0` are the same event. Said here so the two cases cannot
    // drift apart from the reference.
    let oneshot = change.flags & EV_ONESHOT != 0 || is_zero(&interval);
    // A re-`EV_ADD` on the same `(ident, EVFILT_TIMER)` re-arms the timer that
    // is already there, which is what the comment in `SetWakeupTimerEvent` says
    // it is for: "This updates the timer if it already exists in |kqueue_|".
    let fd = match state.timers.get(&ident) {
        Some(timer) => Arc::clone(&timer.fd),
        None => {
            // SAFETY: a plain clock timer; the flags keep it from inheriting a
            // descriptor or blocking this thread.
            let raw = unsafe {
                libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK)
            };
            if raw < 0 {
                return Err(crate::errno::from_host(crate::errno::get()));
            }
            Arc::new(TimerFd(raw))
        }
    };
    let timer = Timer {
        fd,
        interval,
        oneshot,
        enabled: true,
        udata: change.udata,
        fflags: change.fflags,
    };
    timer.arm()?;
    state.timers.insert(ident, Arc::new(timer));
    Ok(())
}

/// The `mach_port_t` in an `ident`.
/// zero-initialises the high half - but a name with bit 32 set is not a name
/// this kernel could issue, so the high half is dropped rather than trusted.
/// Truncating also keeps a name from being confused with a descriptor, which is
/// what the same field means for the other filters.
fn name_of(ident: u64) -> Name {
    ident as u32
}

/// The filter's name, for a refusal. Anything not named here stays its number:
/// inventing a name for a filter this file has never heard of is how a
/// diagnostic starts lying.
fn filter_name(filter: i16) -> &'static str {
    match filter {
        EVFILT_READ => "EVFILT_READ",
        EVFILT_WRITE => "EVFILT_WRITE",
        EVFILT_AIO => "EVFILT_AIO",
        EVFILT_VNODE => "EVFILT_VNODE",
        EVFILT_PROC => "EVFILT_PROC",
        EVFILT_SIGNAL => "EVFILT_SIGNAL",
        EVFILT_TIMER => "EVFILT_TIMER",
        EVFILT_MACHPORT => "EVFILT_MACHPORT",
        EVFILT_FS => "EVFILT_FS",
        EVFILT_USER => "EVFILT_USER",
        EVFILT_VM => "EVFILT_VM",
        EVFILT_EXCEPT => "EVFILT_EXCEPT",
        _ => "a filter kevent64 does not define",
    }
}

/// The filter's `flags` word, spelled out.
///
/// Only the action bits, because only they say what the change *asks for*:
/// `EV_ADD` is "start watching", `EV_DELETE` is "stop watching", and the
/// difference is the whole of whether a refusal costs anything. A word with no
/// action bit in it is not a change at all, and saying so is better than
/// printing a hex number the reader has to decode.
fn flags_meaning(flags: u16) -> String {
    const NAMES: [(u16, &str); 10] = [
        (EV_ADD, "EV_ADD"),
        (EV_DELETE, "EV_DELETE"),
        (EV_ENABLE, "EV_ENABLE"),
        (EV_DISABLE, "EV_DISABLE"),
        (EV_ONESHOT, "EV_ONESHOT"),
        (EV_CLEAR, "EV_CLEAR"),
        (EV_RECEIPT, "EV_RECEIPT"),
        (EV_DISPATCH, "EV_DISPATCH"),
        (EV_ERROR, "EV_ERROR"),
        (EV_EOF, "EV_EOF"),
    ];
    let mut out = String::new();
    for (bit, name) in NAMES {
        if flags & bit != 0 {
            if !out.is_empty() {
                out.push('|');
            }
            out.push_str(name);
        }
    }
    if out.is_empty() {
        out.push_str("no action bit");
    }
    out.push_str(&format!(" ({flags:#06x})"));
    out
}

/// What the `ident` of a refused change actually is in this runtime.
///
/// **This is the question the old message answered by assertion.** It said "a
/// Mach port is not a descriptor" about every `ident` it was handed, and that
/// is a claim about the *filter*, not about the *number* - and the number is
/// the thing that decides whether the refusal is right. The guest's own
/// `EVFILT_READ` changes carry whatever its caller had in a register, which on
/// this host can be a real descriptor number (`kqueue()` hands back the
/// readiness descriptor itself), so "it is a Mach port" is not something this
/// function is entitled to say without looking.
///
/// **`pub` so that [`crate::kqueue`] uses this one derivation rather than a
/// second copy of it**, for the same reason it takes `EVFILT_PROC` and `Proc`
/// from here: two classifiers that could drift would be two places for a
/// diagnostic to start lying.
///
/// So it looks, in this order, and says which test it used:
///
/// * a live receive right in this task's port space - a Mach port;
/// * a port set in this task's space - a Mach port set;
/// * not a name in this task's space, but an open descriptor on this host -
///   a real descriptor, which is the case where the truthful answer exists and
///   is `EVFILT_READ` as poll readiness, in [`crate::kqueue`];
/// * neither, so the number names nothing here at all.
pub fn describe_ident(ident: u64) -> String {
    let name = name_of(ident);
    if space::receiver(name).is_some() {
        return format!("a live receive right in this task's Mach port space (port {name:#x})");
    }
    if let Some(members) = space::members(name) {
        return format!(
            "a Mach port set in this task's space (set {name:#x}, {} member(s))",
            members.len()
        );
    }
    match c_int::try_from(ident) {
        Ok(fd) if unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0 => format!(
            "NOT a Mach port in this task's space, but an open descriptor ({fd}) on this host - so \
             this filter could be answered, as poll readiness, by _kevent rather than by kevent64"
        ),
        Ok(fd) => format!(
            "neither a Mach port in this task's space nor an open descriptor here ({fd} is closed \
             or was never open)"
        ),
        Err(_) => format!(
            "not a Mach port in this task's space, and not a descriptor number this host could \
             have issued either ({ident:#x} is larger than any fd)"
        ),
    }
}

/// The frames above a census line's own, for tying a call shape to a call site.
///
/// **A deliberate exception to "never walk the frame", and gated twice.** The
/// alternatives are all fragile - "assume no prologue was emitted" is not a
/// property Rust promises, and this project has been bitten by assuming things
/// about codegen before - whereas `std`'s walker is the one implementation that
/// knows about omitted frame pointers. It is reached only from a census line,
/// and only on the first sighting of a shape, so the cost is never paid on a
/// path that matters.
///
/// **What it prints is the host's frames, and under the native backend the host's
/// *is* the guest's** - that identity is the basis of this crate's reasoning
/// about `thread_get_state` too. So a frame that resolves to no host symbol is
/// guest code, and its address is the guest's own, to be looked up in the image.
///
/// It is printed through `Display`, which is the stable interface; `frames()` and
/// `ip()` are still unstable, and this file is not going to be the reason a build
/// needs a nightly. A symbolicated frame reads
/// `   3: 0x5da4eec - <unknown>`, and the `3:` is the count of frames above
/// this runtime's own - which is what makes the call site findable.
fn announce_call_site() {
    eprintln!(
        "rosetta3: kevent64: frames above this line, innermost first. A frame with no symbol is \
         guest code; its address is the guest's own and is one of the 21 call sites named in \
         crate::mach::kevent64's module note.\n{frames}",
        frames = std::backtrace::Backtrace::force_capture(),
    );
}

/// Whether the run asked for the kevent census.
///
/// Read **once** through a `OnceLock`, because the answer cannot change and this
/// is on the path of every change. With the variable unset - the default - the
/// census is one atomic load and a `return`.
fn census_wanted() -> bool {
    use std::sync::OnceLock;
    static WANTED: OnceLock<bool> = OnceLock::new();
    *WANTED.get_or_init(|| std::env::var_os("ROSETTA3_TRACE_KEVENT").is_some())
}

/// One line per distinct **shape** of `kevent64` call, with a running count.
///
/// **Off by default; env-gated on `ROSETTA3_TRACE_KEVENT`.** It is the runtime
/// half of the census in this module's note, and it exists because the static
/// half cannot answer the one question that decides whether a refusal is a
/// wall: *did this run reach it at all?* A filter the guest registers nowhere
/// in the image and a filter the guest registers on the first line of start-up
/// look identical in a disassembly.
///
/// "Shape" is everything about the call that changes what it asks for: the
/// filter and flags of the first change, whether a change list was submitted at
/// all, whether there was room for an event (which is what makes the call a
/// **wait** rather than a registration), and whether a timeout was given. A
/// `nevents == 0` call cannot block, and that is the single fact this file's
/// refusals turn on.
///
/// The count is printed on the first sighting of a shape and then at every
/// power of two, so the number converges without a log line per call.
fn announce_call_shape(
    changes: &[Kevent64],
    nchanges: i32,
    nevents: i32,
    flags: u32,
    immediate: bool,
    timeout: *const libc::timespec,
) {
    if !census_wanted() {
        return;
    }
    type Key = (i16, u16, u32, bool, bool, bool, bool);
    let first = changes.first();
    let key: Key = (
        first.map_or(0, |c| c.filter),
        first.map_or(0, |c| c.flags),
        first.map_or(0, |c| c.fflags),
        nchanges > 0,
        nevents > 0,
        !timeout.is_null(),
        immediate,
    );
    use std::sync::OnceLock;
    static SEEN: OnceLock<Mutex<HashMap<Key, u32>>> = OnceLock::new();
    let nth = {
        let mut seen = SEEN
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let slot = seen.entry(key).or_insert(0);
        *slot += 1;
        *slot
    };
    // First sighting, then every doubling: 1, 2, 4, 8, ... 16, 32, 64, ...
    // The frames are printed only on the *first* sighting: a backtrace costs
    // real time, and one per shape is enough to tie a shape to a call site.
    let first_time = nth == 1;
    if !nth.is_power_of_two() {
        return;
    }
    if first_time {
        announce_call_site();
    }
    let filter = match first {
        Some(c) => format!(
            "{} ({}) with {}",
            filter_name(c.filter),
            c.filter,
            flags_meaning(c.flags)
        ),
        None => "no change list at all".to_string(),
    };
    eprintln!(
        "rosetta3: kevent64: {nth} call(s) of this shape - {filter}, fflags {:#x}, \
         {nchanges} change(s), {nevents} event slot(s) so {}, timeout {}, \
         the flags word {flags:#x} so KEVENT_FLAG_IMMEDIATE is {}. {}",
        first.map_or(0, |c| c.fflags),
        if nevents > 0 {
            "this call can block, so anything refused in it is a possible hang"
        } else {
            "this call cannot block, so nothing refused in it can hang a thread"
        },
        if timeout.is_null() {
            "NULL, which means wait forever"
        } else if is_zero(unsafe { &*timeout }) {
            "a zero timespec, which means do not wait"
        } else {
            "a deadline"
        },
        if immediate { "set" } else { "clear" },
        if nchanges > 0 && first.is_some_and(|c| c.flags & EV_ADD == 0) {
            "The change list carries no EV_ADD, so this is a deregistration: the registration it \
             names was never made by this call."
        } else {
            "The change list carries EV_ADD, so this call does register something."
        },
    );
}

/// How many times each filter has been refused by this file.
///
/// **Per filter, and not one `Once` for the whole set.** A single `Once` is the
/// worst shape this refusal can take: the guest gets `EINVAL` as an `EV_ERROR`
/// event's `data` and *we* get one line, so a run that refuses `EVFILT_READ`
/// twenty-six times says so once, and a run that goes on to refuse
/// `EVFILT_VNODE` says nothing about that at all. The count is what turns "this
/// happened" into "this happened twenty-six times, and here is the number".
fn refusal_count(filter: i16) -> u32 {
    use std::sync::OnceLock;
    static COUNTS: OnceLock<Mutex<HashMap<i16, u32>>> = OnceLock::new();
    let counts = COUNTS.get_or_init(Default::default);
    let mut counts = counts.lock().unwrap_or_else(|e| e.into_inner());
    let slot = counts.entry(filter).or_insert(0);
    *slot += 1;
    *slot
}

/// A filter this file will not answer, **named, per filter, and counted**.
///
/// The `Once` this replaces printed one paragraph the first time any filter was
/// refused and was silent about every other one. Two things were wrong with
/// that. It could not say *how often*, which is the difference between a
/// diagnostic and a wall. And it asserted what the `ident` was; see
/// [`describe_ident`].
fn refused_filter(change: &Kevent64) -> i32 {
    let (filter, ident) = (change.filter, change.ident);
    let nth = refusal_count(filter);
    // The first few one at a time, because twenty-six is a fact worth having
    // exactly; after that every thousandth, so a run that refuses in a loop
    // cannot turn the log into the wall.
    if nth <= 8 || nth % 1000 == 0 {
        eprintln!(
            "rosetta3: kevent64 refused {} ({filter}) with {} on ident {ident:#x}, which is {} \
             [refusal {nth} of this filter in this process]. This kevent64 answers EVFILT_MACHPORT \
             (-8) - \"is there a message on this port\", including receiving one into the caller's \
             buffer - EVFILT_TIMER (-7) on a real timerfd, and EVFILT_PROC (-5) on a real pidfd; \
             every other filter is refused here by name. The whole-crate census of what the guest \
             passes, and which of it is waited on, is in crate::mach::kevent64's module note",
            filter_name(filter),
            flags_meaning(change.flags),
            describe_ident(ident),
        );
    }
    crate::errno::from_host(libc::EINVAL)
}

/// A receive option other than [`MACH_RCV_MSG`] in an `EVFILT_MACHPORT`
/// `fflags`, named.
///
/// This is a much narrower refusal than the one it replaces. The one it
/// replaces refused *every* non-zero `fflags` on the strength of a claim about
/// `<sys/event.h>` that the header does not make; this refuses only the bits
/// that would change what the receive has to do and that this file cannot do.
///
/// `MACH_RCV_LARGE` is the one that earns its place here rather than being
/// pedantry. With it, `mach_msg` **keeps** a message too large for the caller's
/// buffer and answers `MACH_RCV_TOO_LARGE`, leaving the port's queue holding a
/// message that is no longer readable - so the descriptor this filter polls
/// goes quiet while the message is still on it, and there is nothing left to
/// wake the wait. The event would fire once and then never again, which is a
/// readiness that lies by going silent. `super::msg` keeps such a message in
/// the port's `held` slot for the *next* receive, and this filter has no
/// `held`-aware poll to notice it, so the bit is refused rather than
/// half-honoured.
fn refused_receive_option(ident: u64, fflags: u32) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused receive options {fflags:#x} on ident {ident:#x}: \
             MACH_RCV_MSG ({:#x}) is answered, and it is the whole of what this answers. \
             MACH_RCV_LARGE ({MACH_RCV_LARGE:#x}) is not, because it makes mach_msg keep a \
             message that does not fit instead of destroying it, and a message that is on the \
             port but no longer readable is a readiness this filter could not report again",
            MACH_RCV_MSG
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// `MACH_RCV_MSG` with no buffer of the caller's own to receive into.
fn refused_receive_buffer(ident: u64, ext: [u64; 2]) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused a MACH_RCV_MSG registration of {ident:#x} with \
             ext[0] = {:#x} and ext[1] = {:#x}: a zero length means <sys/event.h> looks for \
             the receive buffer in the kevent_qos output area the system call was given, and \
             this kevent64 has no such area - a null buffer has nowhere to put a message, so \
             the event would report one that was never taken",
            ext[0], ext[1]
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// `MACH_RCV_MSG` on a port *set*.
///
/// The header's "Only portsets are supported at this time" line is about which
/// idents the filter takes, not about which idents the receive option takes -
/// and the guest's own registration uses the option on a plain receive right,
/// so that line cannot be read the way a refusal needs it read. What is true
/// is narrower: a set's event reports the *member* that had the message, and a
/// receive has to name one port. Which member the caller meant is not in the
/// `kevent64_s`, and guessing would put a message in a buffer the caller did
/// not offer it for.
fn refused_receive_on_a_set(ident: u64, set: Name) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused a MACH_RCV_MSG registration of port set {set:#x} \
             (ident {ident:#x}): receiving during delivery names one port, and a set's event \
             reports whichever member happened to have the message - which member is not in \
             the kevent64_s. No registration in the framework asks for this; registering it \
             anyway would receive a member's message into a buffer chosen by guesswork"
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// A registration of something that is not a port with a queue.
fn refused_ident(name: Name) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused an EVFILT_MACHPORT registration of {name:#x}: it is not a \
             receive right or a port set in this task's space. EVFILT_MACHPORT's ident is a \
             mach_port_t, not a descriptor, so a value that names neither cannot be watched - and \
             answering anyway would be a knote that can never fire"
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// An action bit an `EVFILT_TIMER` has no meaning for, named.
fn refused_timer_action(flags: u16, ident: u64) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused flags {flags:#x} on an EVFILT_TIMER with ident {ident:#x}: \
             the bits outside {TIMER_SERVED:#x} are the filter's own EV_FLAG0 and EV_FLAG1, which \
             mean nothing to a timer, and EV_SYSFLAGS, which the kernel reserves"
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// A timer epoch this host does not have, named.
fn refused_timer_epoch(ident: u64, fflags: u32) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused an EVFILT_TIMER with fflags {fflags:#x} on ident \
             {ident:#x}: NOTE_MACHTIME ({NOTE_MACHTIME:#x}) and NOTE_MACH_CONTINUOUS_TIME \
             ({NOTE_MACH_CONTINUOUS_TIME:#x}) are mach_absolute_time and mach_continuous_time, \
             which have no counterpart here, and NOTE_ABSOLUTE ({NOTE_ABSOLUTE:#x}) is a \
             calendar deadline that XNU freezes at registration so an NTP step cannot move it - \
             a CLOCK_REALTIME timerfd deadline is moved by one. The interval form is answered"
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// An action bit an `EVFILT_PROC` has no meaning for, named.
fn refused_proc_action(flags: u16, ident: u64) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused flags {flags:#x} on an EVFILT_PROC with ident {ident:#x}: \
             the bits outside {PROC_SERVED:#x} are EV_EOF ({EV_EOF:#x}) and EV_ERROR \
             ({EV_ERROR:#x}), which the kernel returns rather than accepts, and EV_SYSFLAGS, which \
             it reserves. The registration EV_ADD|EV_CLEAR with fflags NOTE_EXIT is answered"
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// A `NOTE_` an `EVFILT_PROC` cannot report, named.
///
/// This is the refusal that keeps the filter honest. A `pidfd` becomes readable
/// when a process becomes a zombie and reports **one** fact, so
/// [`NOTE_EXIT`] is the whole of what can be answered - and the two notes that
/// would put something *in* `data` are the ones that cannot be:
///
/// * `NOTE_EXITSTATUS` (`0x04000000`) is the one `<sys/event.h>` documents as
///   "exit status to be returned, valid for child process or when allowed to
///   signal target pid". XNU fills it by reading the dying `struct proc`'s
///   exit status. A `pidfd` cannot: the only way to learn a process's exit
///   status is to reap it, and reaping is precisely what must not happen here -
///   `base::WaitpidWithTimeout` collects the framework's own children with
///   `waitpid`, and a `pidfd` that stole the status would break every one of
///   them.
/// * `NOTE_EXIT_DETAIL` (`0x02000000`) is the jetsam / `CSKILLED` /
///   decrypt-fail reason, which is a field of a macOS `struct proc` that has no
///   counterpart in a Linux one.
///
/// And the lifecycle notes - `NOTE_FORK`, `NOTE_EXEC`, `NOTE_REAP`,
/// `NOTE_SUSPEND`, `NOTE_RESUME`, the `NOTE_LINK` family - are a macOS process
/// lifecycle this runtime does not model, so a `pidfd` cannot report them
/// either. **Refused, not ignored and not answered with a zero.**
fn refused_proc_note(ident: u64, fflags: u32) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused fflags {fflags:#x} on an EVFILT_PROC with ident \
             {ident:#x}: NOTE_EXIT ({NOTE_EXIT:#x}) is answered, on a real pidfd that the kernel \
             makes readable when the process becomes a zombie. NOTE_EXITSTATUS \
             ({NOTE_EXITSTATUS:#x}) and NOTE_EXIT_DETAIL ({NOTE_EXIT_DETAIL:#x}) are not, because \
             a pidfd reports that a process exited and nothing about how - the exit status is only \
             obtainable by reaping it, which is what the framework's own waitpid calls collect, and \
             the detail bits are fields of a macOS struct proc this host does not have. The \
             remaining fflags are the NOTE_FORK / NOTE_EXEC / NOTE_REAP / NOTE_SUSPEND / \
             NOTE_RESUME and NOTE_LINK lifecycle, which is a process model rather than a \
             descriptor. A zero in data under any of them would be a fabricated answer"
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// A kernel with no `pidfd` at all, named.
///
/// `pidfd_open` is Linux 5.3 and this host is 7.2.6, so this is not expected to
/// print. It exists because the alternative to printing it is answering
/// `EVFILT_PROC` with a poll loop, and a poll loop is a different primitive
/// wearing this filter's name - the thing this file is written to avoid.
pub fn refused_proc_unavailable(ident: u64) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64: this kernel has no pidfd (pidfd_open returned ENOSYS for ident \
             {ident:#x}), so EVFILT_PROC is refused rather than answered by polling. pidfd_open is \
             Linux 5.3; the alternative - a timer and a kill(pid, 0) loop - would be a different \
             primitive under this filter's name, and a process that exits and is reaped inside one \
             iteration of it would be missed entirely"
        );
    });
}

/// An `EV_DELETE` of an `EVFILT_PROC` that was never registered.
/// `pub` because [`crate::kqueue`] refuses the same `ENOENT` from its own
/// registration table, and the name of the missing registration is the same
/// thing in both.
pub fn announce_missing_proc(ident: u64) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64: ENOENT for an EVFILT_PROC with ident {ident:#x} - no such \
             registration on this queue, and so no pidfd to close. Darwin answers the same way, and \
             it is the check that catches a delete of a registration that was never made"
        );
    });
}

///
/// Named, once, and not refused - because the reference implementation does not
/// read it either. `filt_timervalidate` in XNU's `kern_event.c` switches on the
/// four unit bits, `NOTE_LEEWAY`, `NOTE_ABSOLUTE`, `NOTE_MACHTIME` and
/// `NOTE_MACH_CONTINUOUS_TIME`, and on nothing else; `NOTE_CRITICAL`,
/// `NOTE_BACKGROUND`, `NOTE_ADD` and `NOTE_FORWARD` are coalescing and
/// direction hints that reach the timer machinery without changing what a
/// caller can observe about when the event arrives. Ignoring a bit the
/// reference also ignores is matching it; ignoring a bit the reference acts on
/// would not be, and that is the line this file is drawn on.
///
/// A `NOTE_` bit that is not part of what a timer means.
///
/// Named, once, and not refused - because the reference implementation does not
/// read it either. `filt_timervalidate` in XNU's `kern_event.c` switches on the
/// four unit bits, `NOTE_LEEWAY`, `NOTE_ABSOLUTE`, `NOTE_MACHTIME` and
/// `NOTE_MACH_CONTINUOUS_TIME`, and on nothing else; `NOTE_CRITICAL`,
fn announce_ignored_timer_hint(ident: u64, fflags: u32) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64: an EVFILT_TIMER with fflags {fflags:#x} on ident {ident:#x} \
             carries bits outside the {TIMER_MEANINGFUL:#x} that XNU's filt_timervalidate \
             switches on. They are coalescing and direction hints there too and do not change \
             when the event arrives, so they are not acted on - the same answer the reference \
             gives, said here so it is not silent"
        );
    });
}

/// An `EV_DELETE` of a timer that is not armed.
fn announce_missing_timer(ident: u64) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64: ENOENT for an EVFILT_TIMER with ident {ident:#x} - no such \
             timer on this queue. Darwin answers the same way, and it is the check that catches a \
             delete of a timer that was never armed"
        );
    });
}

/// A change naming a registration that is not there.
fn announce_missing_registration(name: Name, flags: u16) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64: ENOENT for {name:#x} with flags {flags:#x} - no such \
             registration on this queue. Darwin answers the same way, and it is the check that \
             catches a filter applied to a queue that was never told about it"
        );
    });
}

/// An action bit a Mach port filter has no meaning for, named.
fn refused_action(flags: u16, ident: u64) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused flags {flags:#x} on ident {ident:#x}: the bits outside \
             {SERVED:#x} are the filter's own EV_FLAG0 and EV_FLAG1, which mean nothing to a Mach \
             port, and EV_SYSFLAGS, which the kernel reserves. Ignored rather than refused would \
             be a registration doing something other than what it says"
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// A `flags` word this file does not act on, named.
///
/// `KEVENT_FLAG_IMMEDIATE` is **not** refused: it is `<sys/event.h>`'s
/// "immediate timeout" and it is served, as a zero timeout. What is left in
/// the word is one of two things, and both are refused for the same reason -
/// each asks for a delivery path this filter does not have:
///
/// * `KEVENT_FLAG_ERROR_EVENTS` (`0x2`), "output events only include change
///   errors". On Darwin that is the path where a change the kernel *rejected*
///   comes back as an `EV_ERROR` event rather than as a return value, which is
///   the same `EV_RECEIPT` machinery this file already answers for the
///   `kevent64` shape - but Darwin's rule for which rejected changes are
///   reported, and which are reported at all, is not in the public header, and
///   guessing it would report errors the caller did not get and hide ones it
///   did.
/// * The private workloop and user-data bits (`KEVENT_FLAG_WORKLOOP`,
///   `KEVENT_FLAG_WORKQ`, `KEVENT_FLAG_STACK_DATA`, and the rest in
///   `<sys/event_private.h>`), which name a `kevent_qos` argument and a queue
///   this `kevent64` has never been given.
fn refused_flags(flags: u32) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused flags {flags:#x} ({}) on the call's flags argument. \
             KEVENT_FLAG_IMMEDIATE ({KEVENT_FLAG_IMMEDIATE:#x}, \"immediate timeout\") is \
             answered, as a zero timeout. KEVENT_FLAG_ERROR_EVENTS \
             ({:#x}, \"output events only include change errors\") is not, because which rejected \
             changes Darwin reports is not in the public header and guessing it would report \
             errors the caller did not get; the rest are the private workloop and user-data bits \
             in <sys/event_private.h>, which name a kevent_qos argument this call was not given",
            flags & !KEVENT_FLAG_IMMEDIATE,
            0x0000_0002u32,
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// One descriptor to wait on, and the registration and port it belongs to.
struct Watch {
    fd: c_int,
    /// The port that actually has the message: the port itself, or the member
    /// of a port set. This is what `data` reports.
    source: Name,
    ident: Name,
    registration: Registration,
}

/// One armed `EVFILT_TIMER` to wait on.
///
/// The `Arc<Timer>` is what keeps the descriptor alive for the length of the
/// `poll`, the same way [`space::Receiver`] keeps a port's socket alive in
/// [`Watch`]: an `EV_DELETE` on another thread drops the queue's own `Arc`, and
/// without a second one the descriptor would be closed underneath the wait.
struct TimerWatch {
    fd: c_int,
    ident: u64,
    timer: Arc<Timer>,
}

/// One `EVFILT_PROC` to wait on.
///
/// The `Arc<Proc>` is what keeps the `pidfd` alive for the length of the
/// `poll`, for the same reason [`TimerWatch`] keeps the `timerfd` and
/// [`Watch`] keeps a port's socket: an `EV_DELETE` on another thread drops the
/// queue's own `Arc`, and without a second one the descriptor would be closed
/// underneath the wait. **Cancelling a registration closes the `pidfd`**, and
/// this is the reference that decides *when*.
struct ProcWatch {
    fd: c_int,
    ident: u64,
    proc: Arc<Proc>,
}

/// The `EVFILT_PROC` registrations to wait on.
///
/// A separate function from [`watches`] rather than another element of its
/// tuple, because that function mutates - it prunes registrations whose port is
/// gone - and a process that has exited is not that: its knote is **gone**, and
/// the `pidfd` that says so stays readable for as long as the descriptor lives,
/// which is until the registration is removed. So there is nothing to prune and
/// nothing to add to `watches`' tuple, and the shared `poll` set below gains a
/// third chain rather than a rewrite.
fn proc_watches(state: &Queue) -> Vec<ProcWatch> {
    state
        .procs
        .iter()
        .filter(|(_, proc)| proc.enabled)
        .map(|(ident, proc)| ProcWatch {
            fd: proc.fd.0,
            ident: *ident,
            proc: Arc::clone(proc),
        })
        .collect()
}

/// The descriptors to wait on, and the registrations whose port is gone.
fn watches(state: &mut Queue) -> (Vec<Watch>, Vec<TimerWatch>, Vec<Registration>) {
    // A registration whose port has been destroyed has no descriptor and no way
    // to be woken, so it is taken out here rather than waited on forever. There
    // is nothing to hook the destruction itself: `mach_port_destroy` is not this
    // module's.
    // The registration is *copied out* first, because reporting it to the
    // caller that was waiting for it is the whole point of taking it out.
    let mut vanished = Vec::new();
    state.registrations.retain(|name, registration| {
        let keep = space::receiver(*name).is_some() || space::members(*name).is_some();
        if !keep {
            vanished.push(*registration);
        }
        keep
    });
    let mut out = Vec::new();
    for registration in state.registrations.values() {
        if !registration.enabled {
            continue;
        }
        for source in ports_behind(registration.ident).unwrap_or_default() {
            // The `Receiver` is held for as long as the wait is, so the
            // descriptor cannot be closed underneath the `poll` below.
            let Some(port) = space::receiver(source) else {
                continue;
            };
            out.push(Watch {
                fd: port.fd(),
                source,
                ident: registration.ident,
                registration: *registration,
            });
        }
    }
    // A disabled timer is armed with no deadline, so it is not polled: its
    // descriptor could not become readable and waiting on it would only cost
    // the `poll` a slot.
    let timers: Vec<TimerWatch> = state
        .timers
        .iter()
        .filter(|(_, timer)| timer.enabled)
        .map(|(ident, timer)| TimerWatch {
            fd: timer.fd.0,
            ident: *ident,
            timer: Arc::clone(timer),
        })
        .collect();
    (out, timers, vanished)
}

/// `int kevent64(int kq, const struct kevent64_s *changelist, int nchanges,
/// struct kevent64_s *eventlist, int nevents, unsigned int flags, const struct
/// timespec *timeout)`
///
/// The `flags` argument is a **value**, not a pointer, and the `timeout` is the
/// pointer: `<sys/event.h>` declares them in that order and the framework's own
/// call at `+0x5d9a9db` loads `xor r9d,r9d` for one and `mov QWORD PTR
/// [rsp],0x0` for the other, which is the only way to tell the two apart.
///
/// # Safety
///
/// `changelist` has `nchanges` readable events and `eventlist` has room for
/// `nevents` writable ones.
pub unsafe extern "C" fn kevent64(
    queue: c_int,
    changelist: *const Kevent64,
    nchanges: c_int,
    eventlist: *mut Kevent64,
    nevents: c_int,
    flags: u32,
    timeout: *const libc::timespec,
) -> c_int {
    if nchanges < 0 || nevents < 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    if (nchanges != 0 && changelist.is_null()) || (nevents != 0 && eventlist.is_null()) {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    if flags & !KEVENT_FLAG_IMMEDIATE != 0 {
        crate::errno::set(crate::errno::to_host(refused_flags(flags)));
        return -1;
    }
    // `KEVENT_FLAG_IMMEDIATE` is `<sys/event.h>`'s "immediate timeout": the
    // call collects whatever is ready and does not block. It is not a request
    // for a different kind of event, and it is not the workloop or user-event
    // machinery, so it collapses to a zero timeout and nothing else.
    let immediate = flags & KEVENT_FLAG_IMMEDIATE != 0;
    if !is_queue(queue) {
        crate::errno::set(libc::EBADF);
        return -1;
    }
    // `kqueue`'s own answer is not a filtered set, so a queue this module has
    // never been asked about is an empty one rather than a refusal. The
    // descriptor was just proved to be a kqueue, which is the only thing about
    // it that decides whether `kevent64` refuses.
    let state = queues()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(queue)
        .or_insert_with(|| Arc::new(Mutex::new(Queue::default())))
        .clone();
    let capacity = nevents as usize;
    let mut out: Vec<Kevent64> = Vec::with_capacity(capacity);

    // SAFETY: the caller states there are `nchanges` changes to read.
    let changes = match nchanges {
        0 => &[][..],
        n => unsafe { std::slice::from_raw_parts(changelist, n as usize) },
    };
    // The runtime census reports the *shape* of each call, and the frames above
    // it, so that a shape can be tied to the call site that produced it. All 21
    // call sites are named with their addresses in this module's note, so a
    // frame list is enough to say which of them a run reached.
    announce_call_shape(changes, nchanges, nevents, flags, immediate, timeout);
    {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        for change in changes {
            let failure = apply(&mut state, change).err();
            // `EV_RECEIPT` asks for the outcome of *every* change rather than
            // for readiness, and it asks whether or not the change failed -
            // which is how a caller applies a batch and finds out which one the
            // kernel rejected.
            if change.flags & EV_RECEIPT != 0 {
                if out.len() < capacity {
                    out.push(Kevent64 {
                        flags: EV_ERROR,
                        data: failure.unwrap_or(0) as i64,
                        ..*change
                    });
                }
                continue;
            }
            if let Some(errno) = failure {
                crate::errno::set(crate::errno::to_host(errno));
                return -1;
            }
        }
    }
    if capacity == 0 {
        return 0;
    }
    if !out.is_empty() {
        // SAFETY: the caller states there is room for `nevents`.
        unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), eventlist, out.len()) };
        return out.len() as c_int;
    }

    let wait = match timeout_ms(timeout) {
        // `KEVENT_FLAG_IMMEDIATE` is "immediate timeout", so it replaces the
        // caller's timeout rather than adding to it: `DoInternalWork` passes a
        // *null* timeout when it wants to be told what is already pending, and
        // a null timeout would otherwise mean "block forever".
        Ok(_) if immediate => 0,
        Ok(ms) => ms,
        Err(errno) => {
            crate::errno::set(crate::errno::to_host(errno));
            return -1;
        }
    };
    let (watched, timers, mut vanished) = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        watches(&mut state)
    };
    // Collected from the same table in the same critical section as `watches`,
    // and for the same reason: the `Arc<Proc>` handed out here is what keeps a
    // `pidfd` alive across the `poll` that follows.
    let procs = {
        let state = state.lock().unwrap_or_else(|e| e.into_inner());
        proc_watches(&state)
    };
    if watched.is_empty() && timers.is_empty() && procs.is_empty() && vanished.is_empty() {
        // Nothing is registered, so nothing can ever become ready.
        //
        // With a timeout that is simply Darwin's answer: nothing happened, and
        // `kevent64` says so by reporting no events. With no timeout at all
        // Darwin blocks here forever, and blocking is refused instead: a caller
        // that is told can say what it asked for, and a caller that is not told
        // gets a hang with nothing in the log to explain it. An *immediate* call
        // cannot block, so it is the first case and not the second: it collects
        // and reports nothing, which is what `rv == 0` means to
        // `DoInternalWork` - "no events to dispatch", not an error.
        if wait < 0 {
            announce_empty_wait();
            crate::errno::set(libc::EINVAL);
            return -1;
        }
        // SAFETY: no descriptors, which `poll` allows, and the caller's own
        // timeout in milliseconds.
        if unsafe { libc::poll(std::ptr::null_mut(), 0, wait) } < 0 {
            crate::errno::set(crate::errno::get());
            return -1;
        }
        return 0;
    }

    let mut entries: Vec<libc::pollfd> = watched
        .iter()
        .map(|watch| libc::pollfd {
            fd: watch.fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .chain(timers.iter().map(|timer| libc::pollfd {
            fd: timer.fd,
            events: libc::POLLIN,
            revents: 0,
        }))
        .chain(procs.iter().map(|proc| libc::pollfd {
            fd: proc.fd,
            events: libc::POLLIN,
            revents: 0,
        }))
        .collect();
    // SAFETY: `entries` is exactly the number of live `struct pollfd` the count
    // is derived from, and nothing else touches it.
    let ready = unsafe { libc::poll(entries.as_mut_ptr(), entries.len() as libc::nfds_t, wait) };
    if ready < 0 {
        let errno = crate::errno::get();
        // `EINTR` is reported rather than restarted. Darwin reports it, and
        // Chromium's `HANDLE_EINTR` around this call is written to see it and
        // try again with the same arguments; a restart here would wait longer
        // than the caller asked for.
        crate::errno::set(errno);
        return -1;
    }

    // A port that went away is reported once, and it comes before readiness so
    // that a caller draining a queue finds out about it rather than blocking on
    // it on the next turn.
    for registration in vanished.drain(..) {
        if out.len() < capacity {
            out.push(registration.vanished());
        }
    }
    if ready > 0 {
        let (port_entries, rest) = entries.split_at(watched.len());
        let (timer_entries, proc_entries) = rest.split_at(timers.len());
        let mut ready_watches: Vec<&Watch> = Vec::new();
        for (entry, watch) in port_entries.iter().zip(&watched) {
            if entry.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) == 0
            {
                continue;
            }
            // One event per registration, not per descriptor: a port set with
            // two ready members is one knote that fired, and `data` says which
            // member it was.
            if ready_watches.iter().any(|seen| seen.ident == watch.ident) {
                continue;
            }
            ready_watches.push(watch);
        }
        // Darwin's order among several ready knotes is the order they became
        // ready in, which a port queue does not record. Sorting by `udata`
        // instead makes two runs of the same program hand the caller the same
        // events in the same order, and puts the one `WaitManyImpl` cares about -
        // the lowest index - first even when `nevents` is too small for all of
        // them.
        ready_watches.sort_by_key(|watch| (watch.registration.udata, watch.ident));
        ready_watches.truncate(capacity - out.len());
        // **Only the delivered set receives.** A knote that `nevents` had no
        // room for is not reported, and a message taken out of a port for an
        // event nobody was handed would be a message destroyed with no event to
        // account for it. So the order is: choose, truncate, *then* receive.
        let mut ready_events: Vec<(Name, Kevent64)> = Vec::new();
        for watch in ready_watches {
            let event = match watch.registration.receive {
                // SAFETY: the buffer and its length are the guest's own, taken
                // from its `EV_ADD` and range-checked by [`Receive::deliver`]
                // before either is used.
                Some(receive) => {
                    // SAFETY: as above.
                    let (code, size) = unsafe { receive.deliver(watch.source) };
                    if code != super::kern::MACH_MSG_SUCCESS {
                        announce_failed_receive(
                            watch.source,
                            watch.ident,
                            code,
                            watch.registration.udata,
                            receive.length,
                        );
                    }
                    watch.registration.received(watch.source, code, size)
                }
                None => watch.registration.fired(watch.source),
            };
            ready_events.push((watch.ident, event));
        }
        if !ready_events.is_empty() {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            for (ident, _) in &ready_events {
                // `EV_ONESHOT` deletes the registration once it has fired and
                // `EV_DISPATCH` disables it until it is enabled again. Both are
                // how a caller avoids being handed the same readiness every
                // turn, and neither may happen to a knote that was *not*
                // delivered - so this walks the delivered list, not the ready
                // one.
                let Some(registration) = state.registrations.get(ident).copied() else {
                    continue;
                };
                if registration.flags & EV_ONESHOT != 0 {
                    state.registrations.remove(ident);
                } else if registration.flags & EV_DISPATCH != 0
                    && let Some(registration) = state.registrations.get_mut(ident)
                {
                    registration.enabled = false;
                }
            }
        }
        out.extend(ready_events.into_iter().map(|(_, event)| event));

        // Timers, and only as many as `nevents` still has room for. The same
        // rule as the ports: an event that is not delivered must not consume
        // anything. A `timerfd` that is not read stays readable, so the ones that
        // do not fit are left armed and fire on the next call.
        let room = capacity - out.len();
        let mut ready_timers: Vec<&TimerWatch> = timer_entries
            .iter()
            .zip(&timers)
            .filter(|(entry, _)| {
                entry.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
            })
            .map(|(_, timer)| timer)
            .collect();
        ready_timers.sort_by_key(|timer| timer.ident);
        let mut fired_timers: Vec<u64> = Vec::new();
        for timer in ready_timers.into_iter().take(room) {
            // A `timerfd` reports readiness by becoming readable, and the read
            // is what clears it. **Not reading it is a hot spin**: the
            // descriptor stays readable, so the next `kevent64` reports the same
            // expired deadline again and again. The value read is the count of
            // expiries, which is not what the event reports - `data` is the time
            // remaining, which is zero for a timer that has passed.
            let mut expirations: u64 = 0;
            // SAFETY: a live non-blocking `timerfd`, and a buffer of the size
            // the man page gives for the read.
            let read = unsafe {
                libc::read(
                    timer.fd,
                    std::ptr::from_mut(&mut expirations).cast(),
                    size_of::<u64>(),
                )
            };
            if read < 0 {
                // `EAGAIN` is another thread having read it first, which is the
                // one race this filter has: nothing to report and nothing broken.
                if crate::errno::get() != libc::EAGAIN {
                    announce_timer_read_failed(timer.ident, crate::errno::get());
                }
                continue;
            }
            out.push(timer.timer.fired(timer.ident));
            fired_timers.push(timer.ident);
        }
        if !fired_timers.is_empty() {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            for ident in fired_timers {
                // `filt_timervalidate`'s case C: "Interval timer causes the
                // knote to arm for the next interval unless one-shot is set". A
                // one-shot is gone once delivered - the descriptor has already
                // disarmed itself, so all that is left is to forget it - and a
                // repeating one stays armed, which is what its `it_interval`
                // already does.
                let Some(timer) = state.timers.get(&ident) else {
                    continue;
                };
                if timer.oneshot {
                    state.timers.remove(&ident);
                }
            }
        }

        // `EVFILT_PROC`, and only as many as `nevents` still has room for -
        // the same rule as the timers, and for a sharper reason. A `pidfd`
        // **stays** readable after the process is reaped, for as long as the
        // descriptor lives: there is no read that clears it, because there is
        // nothing in the descriptor but the fact of the exit. So a
        // registration whose event did not fit in `nevents` must not be
        // forgotten, or the fact is dropped on the floor and never reported.
        // Reported-and-forgotten is the rule: the delivered ones go, the rest
        // stay armed and are reported by the next call.
        let room = capacity - out.len();
        let mut ready_procs: Vec<&ProcWatch> = proc_entries
            .iter()
            .zip(&procs)
            .filter(|(entry, _)| {
                entry.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
            })
            .map(|(_, proc)| proc)
            .collect();
        // The same ordering argument as the ports: Darwin's order among several
        // ready knotes is the order they became ready in, which a `pidfd` does
        // not record, and sorting by `ident` makes two runs of the same program
        // hand the caller the same events in the same order.
        ready_procs.sort_by_key(|proc| proc.ident);
        let fired_procs: Vec<u64> = ready_procs
            .into_iter()
            .take(room)
            .map(|proc| {
                out.push(proc.proc.fired());
                proc.ident
            })
            .collect();
        if !fired_procs.is_empty() {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            for ident in fired_procs {
                // `filt_procevent` sets `EV_ONESHOT` on `NOTE_EXIT`, and the
                // knote was attached to a `struct proc` that no longer exists,
                // so there is nothing left to wait on: the registration is gone
                // once it has been delivered, and dropping it here drops the
                // last `Arc<PidFd>` if this was the last reference, which is
                // the `close`.
                state.procs.remove(&ident);
            }
        }
    }
    // SAFETY: the caller states there is room for `nevents`, and `out` never
    // grew past that.
    unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), eventlist, out.len()) };
    out.len() as c_int
}

/// Say once that an armed `timerfd` could not be read when its deadline passed.
///
/// Not a refusal and not a wrong answer: the deadline did pass and the host
/// kernel did say so, so the event is not reported for this one (a `timerfd` that
/// has not been read stays readable, so the next call will see it). It is said
/// because "the descriptor said it was readable and then the read failed" is
/// exactly the sort of thing that is otherwise invisible.
fn announce_timer_read_failed(ident: u64, errno: i32) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64: an EVFILT_TIMER with ident {ident:#x} became readable and its \
             descriptor then refused the read with errno {errno}. No event is reported for it; \
             the deadline is still armed, so the next wait on this queue sees it again"
        );
    });
}

/// Say once that an `MACH_RCV_MSG` event was delivered whose receive did not
/// return `KERN_SUCCESS`.
///
/// The event was still handed over, because `<sys/event.h>` is explicit that
/// the `fflags` of a delivered event "contains the return code normally returned
/// by `mach_msg()`" - that is where the caller is meant to read it. But a caller
/// that reads only `ident` (which is what `MessagePumpKqueue::ProcessEvents`
/// does) cannot see it, so it is said here rather than left in a field.
fn announce_failed_receive(source: Name, ident: Name, code: i32, udata: u64, length: u64) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 delivered an EVFILT_MACHPORT event for port {source:#x} \
             (registration {ident:#x}, udata {udata}) whose MACH_RCV_MSG receive into a \
             {length}-byte buffer returned {:#x} ({}) - the event says so in its fflags, and the \
             message was not taken. A message does not fit unless it is shorter than {} bytes \
             once the default 8-byte trailer is counted, so this line is also the only \
             measurement of how large the message that arrived was",
            code as u32,
            super::kern::describe(code).unwrap_or("a code mach_error_string has no name for"),
            length.saturating_sub(8),
        );
    });
}

/// Say once that a wait was asked for on a queue with nothing to watch.
fn announce_empty_wait() {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64: an untimed wait on a queue with nothing to watch - no \
             registration on this queue is a Mach port with a queue, so nothing can become ready \
             and no message can arrive to wake it. Darwin blocks here forever; EINVAL is refused \
             instead, because a caller that is told can say what it asked for and a caller that \
             is not told gets a hang with nothing in the log to explain it"
        );
    });
}

// ---------------------------------------------------------------------------
// `EVFILT_VNODE`
// ---------------------------------------------------------------------------

/// The seven public `EVFILT_VNODE` notes, quoted from XNU's `bsd/sys/event.h`:
///
/// ```c
/// #define NOTE_DELETE     0x00000001   /* vnode was removed */
/// #define NOTE_WRITE      0x00000002   /* data contents changed */
/// #define NOTE_EXTEND     0x00000004   /* size increased */
/// #define NOTE_ATTRIB     0x00000008   /* attributes changed */
/// #define NOTE_LINK       0x00000010   /* link count changed */
/// #define NOTE_RENAME     0x00000020   /* vnode was renamed */
/// #define NOTE_REVOKE     0x00000040   /* vnode access was revoked */
/// #define NOTE_NONE       0x00000080   /* No specific vnode event ... */
/// #define NOTE_FUNLOCK    0x00000100   /* vnode was unlocked by flock(2) */
/// #define NOTE_LEASE_DOWNGRADE 0x00000200
/// #define NOTE_LEASE_RELEASE   0x00000400
/// ```
///
/// `0x6f` - and the note that `0x6f` is **six** of the seven, not all seven.
///
/// This file used to call it "the seven from `NOTE_DELETE` to `NOTE_REVOKE`",
/// and that is false in a way that mattered, because the whole narrowing argument
/// was written on the difference. `NOTE_DELETE | NOTE_WRITE | NOTE_EXTEND |
/// NOTE_ATTRIB | NOTE_LINK | NOTE_RENAME | NOTE_REVOKE` is **`0x7f`**; `0x6f` has
/// **`NOTE_LINK` (`0x10`) missing**. So the framework asks for six notes, and the
/// one it leaves out is precisely the one `inotify` cannot carry.
///
/// Verified against the running guest rather than by arithmetic alone: run 245's
/// registration line reads `fflags 0x6f`, the interest it was narrowed to is
/// **`0x6b`**, and exactly **one** note is named as dropped - `NOTE_EXTEND`. The
/// arithmetic says the same thing: `0x6f & VNODE_UNREPORTABLE == 0x4`, not `0x14`.
///
/// The immediates are at `Chromium Framework+0x3a9a9ba`, `+0x3a9aed0`,
/// `+0x3a9af8d`, `+0x5d9e7ea`, `+0x5d9ed00` and `+0x5d9edbd`, and nowhere else -
/// **six sites, one constant**, with the `0xfffc` being `EVFILT_VNODE` and the
/// `0x0061` being `EV_ADD|EV_CLEAR|EV_RECEIPT`. The census behind those six is in
/// this module's note; the positive control for the scan is the `EVFILT_PROC` word
/// `0x8000_0000_0001_fffb`, which the same byte scan finds at exactly the three
/// addresses already claimed for it.
const NOTE_DELETE: u32 = 0x0000_0001;
const NOTE_WRITE: u32 = 0x0000_0002;
const NOTE_EXTEND: u32 = 0x0000_0004;
const NOTE_ATTRIB: u32 = 0x0000_0008;
const NOTE_LINK: u32 = 0x0000_0010;
const NOTE_RENAME: u32 = 0x0000_0020;
const NOTE_REVOKE: u32 = 0x0000_0040;

/// `NOTE_REVOKE`, `pub` for the one decision that is [`crate::kqueue`]'s rather
/// than this file's: `filt_vnode_common` sets `EV_EOF | EV_ONESHOT` on a
/// `NOTE_REVOKE`, so the delivery of that note is the delivery that removes the
/// knote, and only the code doing the delivering can act on that.
pub const VNODE_REVOKE: u32 = NOTE_REVOKE;
const NOTE_NONE: u32 = 0x0000_0080;
const NOTE_FUNLOCK: u32 = 0x0000_0100;
const NOTE_LEASE_DOWNGRADE: u32 = 0x0000_0200;
const NOTE_LEASE_RELEASE: u32 = 0x0000_0400;

/// Every `EVFILT_VNODE` note `<sys/event.h>` gives a name to.
const VNODE_NOTES: u32 = NOTE_DELETE
    | NOTE_WRITE
    | NOTE_EXTEND
    | NOTE_ATTRIB
    | NOTE_LINK
    | NOTE_RENAME
    | NOTE_REVOKE
    | NOTE_NONE
    | NOTE_FUNLOCK
    | NOTE_LEASE_DOWNGRADE
    | NOTE_LEASE_RELEASE;

/// The notes this implementation can report, each with a **measured** `inotify`
/// event behind it.
///
/// This set is not a judgement about what a file watcher needs; it is the set
/// for which this host produces an event that says *the same thing*, and each row
/// of the table in the module note carries the XNU call site that raises the note
/// and the `inotify` row measured on this host that carries the same fact. The
/// complement - [`VNODE_UNREPORTABLE`] - is the set with no such row, and it is
/// **dropped from the registration's interest rather than mapped to the nearest
/// event**, because a note this implementation cannot observe must never appear
/// in a delivered `fflags`.
///
/// The narrowing is safe *for this guest* because of a measurement, not an
/// assumption: see [`VNODE_DROPPED_BY_THE_GUEST`].
const VNODE_REPORTABLE: u32 =
    NOTE_WRITE | NOTE_ATTRIB | NOTE_DELETE | NOTE_RENAME | NOTE_REVOKE;

/// The notes with no `inotify` event that says the same thing.
///
/// **`NOTE_EXTEND` and `NOTE_LINK` are the two this host will not report**, and
/// the reasons are different in kind:
///
/// * `NOTE_EXTEND` ("size increased") is a *narrowing* of `NOTE_WRITE` ("data
///   contents changed"). `inotify` reports one bit, `IN_MODIFY`, for both, and
///   does not say which happened. Reporting `NOTE_EXTEND` on the strength of an
///   `IN_MODIFY` would claim a size change that may not have happened; refusing
///   it claims only what was measured. `NOTE_WRITE` carries the same fact with
///   the wider scope XNU gives it. **The framework does ask for this one**, so it
///   is the one that is actually dropped - see [`VNODE_DROPPED_BY_THE_GUEST`].
/// * `NOTE_LINK` ("link count changed") is folded by `inotify` into the *same*
///   bit as a metadata change, `IN_ATTRIB`, and there is nothing in the event
///   that says which. This one is not even a narrowing, so it cannot be papered
///   over: see [`Vnode::attrib_note`], which resolves it with a `stat` of the
///   link count rather than by picking a bit. **The framework does not ask for
///   it** - `0x6f` leaves `NOTE_LINK` out - so on this guest that ambiguity is
///   never posed, and a link count going up is reported as no note at all.
///
/// The rest are not vnode notes a caller can usefully ask for: `NOTE_NONE` is
/// `<sys/event.h>`'s own "No specific vnode event" bit, and the lease and
/// `flock` notes are advisory requests XNU answers by waking a lease holder or
/// nothing at all, with no filesystem event anywhere near them.
const VNODE_UNREPORTABLE: u32 =
    NOTE_EXTEND | NOTE_LINK | NOTE_NONE | NOTE_FUNLOCK | NOTE_LEASE_DOWNGRADE
    | NOTE_LEASE_RELEASE;

/// The one of [`VNODE_UNREPORTABLE`] the framework's own registration asks for,
/// and a measurement that it costs nothing.
///
/// **`0x6f & VNODE_UNREPORTABLE == 0x4` - one note, not two.** This constant used
/// to be `NOTE_EXTEND | NOTE_LINK` on the strength of "`0x6f` is the seven notes",
/// which is false: `NOTE_LINK` is not in `0x6f` at all. The corrected reading is
/// *better* for the implementation, not worse, and it is worth being exact about
/// why:
///
/// * the guest asks for `NOTE_EXTEND` and does **not** get it, because `IN_MODIFY`
///   does not say the file grew - so this is the one place where the delivered
///   `fflags` is narrower than what the caller asked for, and it is narrower
///   *because of a measurement*;
/// * the guest never asks for `NOTE_LINK`, so the `IN_ATTRIB`-is-ambiguous
///   question it raises in [`Vnode::attrib_note`] is a question **this guest does
///   not pose**. A link count going up produces no note, and nothing in the
///   framework would have looked for one.
///
/// The narrowing is safe *for this guest* because of a measurement of its own
/// delivered-event handling, not an inference from the change list:
/// in `Chromium Framework+[0x5d9efb0, 0x5d9f900)` and its line-for-line twin
/// `+[0x3a9b180, 0x3a9ba20)`, the *only* immediates used with `test`, `and` or
/// `cmp` are
///
/// ```text
///  5d9f1dd  and  edx,0x8      ; NOTE_ATTRIB
///  5d9f2ba  test al,0x61     ; NOTE_DELETE | NOTE_RENAME | NOTE_REVOKE
///  5d9f337  test al,0x2      ; NOTE_WRITE
/// ```
///
/// plus `-1`, `EINTR` (4), `0x7fffffff` and two boolean `test`s on `0x1`. **No
/// `0x4` and no `0x10` anywhere in either body**, and the two bodies produce
/// byte-identical greps at the same line numbers, which is the positive control
/// that the scan covers both.
///
/// A change list is a *filter*: it says what a caller would like, not what it
/// checks on the way out. Dropping the one bit it never checks therefore costs the
/// watcher nothing, and keeps the five it does check honest.
///
/// **`pub` so that [`crate::kqueue`]'s test checks this arithmetic against a
/// constant rather than restating it**, which is the difference between a claim
/// and a claim that can rot.
pub const VNODE_DROPPED_BY_THE_GUEST: u32 = NOTE_EXTEND;

/// `inotify` event bits, from `&lt;sys/inotify.h&gt;`. Named rather than written
/// inline because the whole argument of [`Vnode::translate`] is that each note
/// has a *specific* event behind it, and a hex number cannot be checked against
/// a header.
const IN_ACCESS: u32 = 0x0000_0001;
const IN_MODIFY: u32 = 0x0000_0002;
const IN_ATTRIB: u32 = 0x0000_0004;
const IN_CLOSE_WRITE: u32 = 0x0000_0008;
const IN_CLOSE_NOWRITE: u32 = 0x0000_0010;
const IN_OPEN: u32 = 0x0000_0020;
const IN_MOVED_FROM: u32 = 0x0000_0040;
const IN_MOVED_TO: u32 = 0x0000_0080;
const IN_CREATE: u32 = 0x0000_0100;
const IN_DELETE: u32 = 0x0000_0200;
const IN_DELETE_SELF: u32 = 0x0000_0400;
const IN_MOVE_SELF: u32 = 0x0000_0800;
const IN_UNMOUNT: u32 = 0x0000_2000;
const IN_Q_OVERFLOW: u32 = 0x0000_4000;
const IN_IGNORED: u32 = 0x0000_8000;

/// The `inotify` mask that corresponds to [`VNODE_REPORTABLE`].
///
/// Narrowed the way the note mask is, and for the same reason: `inotify` takes a
/// filter too, and asking for events whose note is not in the caller's interest
/// would be work whose result is then thrown away. `IN_IGNORED` has no mask bit
/// - the kernel sends it whenever it drops a watch - and `IN_UNMOUNT` and
/// `IN_Q_OVERFLOW` are not asked for because neither has a vnode note; both are
/// refused by name in [`Vnode::translate`].
///
/// **Measured, because the accepted set is not the obvious one:** on this host
/// `inotify_add_watch` accepts `0x6fff` and `0xfff` and rejects `0x1fff` and
/// `0xffff` with `EINVAL`, so the "all reportable bits" set is not a mask.
const VNODE_INOTIFY_MASK: u32 = IN_MODIFY
    | IN_ATTRIB
    | IN_MOVED_FROM
    | IN_MOVED_TO
    | IN_CREATE
    | IN_DELETE
    | IN_DELETE_SELF
    | IN_MOVE_SELF;

/// The action bits an `EVFILT_VNODE` is served for.
///
/// The same [`SERVED`] set, and each bit means what it means for the other
/// filters: `EV_CLEAR` is honoured by clearing the accumulated note word after
/// a delivery (XNU's `knote_fill_kevent_with_sdata` does `if (kn->kn_flags &
/// EV_CLEAR) kn->kn_fflags = 0;`), `EV_ONESHOT` and `EV_DISPATCH` by dropping
/// and disabling the registration after one delivery.
const VNODE_SERVED: u16 = SERVED;

/// What one `EVFILT_VNODE` change asks for, decided with no reference to any
/// queue's tables.
///
/// The `EVFILT_VNODE` half of [`VnodeChange`]'s contract, and shaped exactly like
/// [`ProcChange`] because it is the same question: what does this change mean,
/// and is this implementation allowed to answer it.
pub enum VnodeChange {
    /// `EV_ADD`: watch the object behind `ident` for the notes in `mask`.
    Watch {
        /// The `fflags` after the unreportable notes have been dropped, and
        /// never zero - a change that asks only for notes this host cannot
        /// report is [`VnodeChange::Refused`], not a watch that never fires.
        mask: u32,
        /// The bits dropped out of `mask`, so the caller of this function can
        /// name them rather than silently keep a narrower filter.
        dropped: u32,
        /// `EV_ADD` "implies enable", so this is false only when `EV_DISABLE`
        /// was in the same word.
        enabled: bool,
        udata: u64,
    },
    /// `EV_DELETE`: forget the registration, which removes the `inotify` watch.
    Delete,
    /// `EV_ENABLE` or `EV_DISABLE` on a registration that is already there.
    Armed { enabled: bool },
    /// The change was rejected. The number is **Darwin's**, for the same reason
    /// [`ProcChange::Refused`]'s is.
    Refused(i32),
}

/// Decide what an `EVFILT_VNODE` change means, and whether it is allowed.
///
/// **This is the whole of the narrowing decision, and it happens before any
/// `inotify` is touched.** The order matters and is the reference's order:
///
/// 1. An action bit outside [`VNODE_SERVED`] is refused, because a caller that
///    asked for a filter-specific `EV_FLAG0` and got a vnode watch would be
///    holding a subscription it did not ask for.
/// 2. A `NOTE_` bit outside [`VNODE_NOTES`] is refused with `ENOTSUP`, which is
///    what `filt_procattach` gives for a `NOTE_` it does not implement and what
///    a caller can tell apart from a note that is merely narrower than it asked.
/// 3. The unreportable notes are then **dropped** and named, and only a change
///    that asked for *nothing else* is refused - with `EINVAL`, and by name,
///    because a registration that succeeds and can never fire is the one
///    outcome this file is written to avoid.
pub fn vnode_change(ident: u64, flags: u16, fflags: u32, udata: u64) -> VnodeChange {
    if flags & !VNODE_SERVED != 0 {
        return VnodeChange::Refused(refused_vnode_action(flags, ident));
    }
    if fflags & !VNODE_NOTES != 0 {
        return VnodeChange::Refused(refused_vnode_note(ident, fflags));
    }
    let dropped = fflags & VNODE_UNREPORTABLE;
    let mask = fflags & !VNODE_UNREPORTABLE;
    if mask == 0 {
        return VnodeChange::Refused(refused_vnode_mask(ident, fflags));
    }
    if flags & EV_DELETE != 0 {
        return VnodeChange::Delete;
    }
    if flags & (EV_ENABLE | EV_DISABLE) != 0 && flags & EV_ADD == 0 {
        return VnodeChange::Armed {
            enabled: flags & EV_ENABLE != 0,
        };
    }
    VnodeChange::Watch {
        mask,
        dropped,
        enabled: flags & EV_DISABLE == 0,
        udata,
    }
}

/// The `inotify` instance a kqueue's `EVFILT_VNODE` watches live in.
///
/// **One per kqueue, and that is a decision with a reason.** The alternative -
/// one process-wide instance - has to be added to every kqueue that has *any*
/// vnode registration, so an event on queue A's file makes queue B's descriptor
/// readable, and queue B then wakes, finds nothing, and goes back to sleep. That
/// is harmless to this guest and it is still a readiness report about a queue
/// that had no event, which is the shape of thing this project refuses. One
/// instance per queue makes the descriptor that `epoll` reports readable *this
/// queue's own* pending vnode events, and nothing else can put anything on it.
///
/// The map is `wd -> watch`, and the watch is a [`Weak`]: the queue's table owns
/// the [`Vnode`], and this map must not keep one alive after an `EV_DELETE` or
/// after the queue itself has gone. [`Inotify::drain`] upgrades, and drops the
/// entry when the upgrade fails.
pub struct Inotify {
    /// The `inotify` descriptor, `IN_NONBLOCK | IN_CLOEXEC`.
    ///
    /// **Non-blocking because the reader is not the only reader.** Two threads
    /// can be inside `kevent` on the same queue - the wait and a
    /// zero-timeout collection from another task - and a blocking `read` on the
    /// second one would wait for an event that the first is about to consume.
    fd: c_int,
    watches: Mutex<HashMap<c_int, Weak<Vnode>>>,
}

impl Drop for Inotify {
    fn drop(&mut self) {
        // The watches went with the `Vnode`s that own them; this closes the
        // descriptor, which is the only other thing this owns.
        unsafe { libc::close(self.fd) };
    }
}

impl Inotify {
    /// A fresh instance, or the host's `errno`.
    pub fn new() -> Result<Self, i32> {
        // `IN_NONBLOCK | IN_CLOEXEC` = 0o4000 | 0o2000000.
        let fd = unsafe { libc::syscall(libc::SYS_inotify_init1, 0o2004000) } as c_int;
        if fd < 0 {
            return Err(crate::errno::to_host(crate::errno::get()));
        }
        Ok(Self {
            fd,
            watches: Mutex::new(HashMap::new()),
        })
    }

    /// The descriptor to put in a `poll`/`epoll` set.
    pub fn raw_fd(&self) -> c_int {
        self.fd
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<c_int, Weak<Vnode>>> {
        self.watches.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Add `watch` and return its `inotify` watch descriptor.
    fn insert(&self, watch: &Arc<Vnode>) -> c_int {
        let mut watches = self.lock();
        watches.insert(watch.wd, Arc::downgrade(watch));
        watch.wd
    }

    /// Forget `wd`, and stop the kernel sending anything more for it.
    ///
    /// **The order is load-bearing.** Removing the watch from this module's own
    /// map is what makes the `IN_IGNORED` the kernel sends in reply to
    /// `inotify_rm_watch` *not* become a `NOTE_REVOKE`: an `IN_IGNORED` whose
    /// `wd` is no longer in the map is dropped, which is right, because the
    /// object it was about is one the caller has just stopped watching. The
    /// kernel's `IN_IGNORED` means "the watch is gone", and the two reasons a
    /// watch goes - the object went, or the caller said so - are not the same
    /// fact, and only the first is a `NOTE_REVOKE`.
    fn remove(&self, wd: c_int) {
        self.lock().remove(&wd);
        // SAFETY: `wd` came from `inotify_add_watch` on this instance. A
        // failure here means the watch is already gone, which is the state the
        // caller asked for either way, so the number is not worth reporting.
        unsafe { libc::syscall(libc::SYS_inotify_rm_watch, self.fd, wd) };
    }

    /// Read every queued event, and fold each into the pending note word of the
    /// watch it belongs to. Returns how many watches gained a note.
    ///
    /// **An event whose `wd` is not in the map is dropped**, which is the case
    /// described on [`Inotify::remove`] plus the case of a watch whose queue has
    /// gone. It is also the case `IN_Q_OVERFLOW` arrives in, and that one is
    /// named rather than dropped: see [`refused_vnode_overflow`].
    pub fn drain(&self) -> usize {
        let mut buffer = [0u8; 16 * 1024];
        let mut fired = 0usize;
        let mut woken: Vec<Arc<Vnode>> = Vec::new();
        loop {
            let n = unsafe {
                libc::read(
                    self.fd,
                    buffer.as_mut_ptr().cast::<c_void>(),
                    buffer.len(),
                )
            };
            if n <= 0 {
                // `EAGAIN` is the normal end: the descriptor is non-blocking and
                // there is nothing left. Anything else is this host failing to
                // hand over its own events, and it is named below.
                if n < 0 {
                    // The host's own number, and it is compared with the host's
                    // own `EAGAIN` - not translated, because the translation is
                    // in the other direction and a `from_host` here would make
                    // the normal end of every drain look like a failure.
                    let errno = crate::errno::get();
                    if errno != libc::EAGAIN {
                        refused_vnode_read(errno);
                    }
                }
                break;
            }
            let mut offset = 0usize;
            while offset + 16 <= n as usize {
                let (wd, mask, _cookie, len) = {
                    let b = &buffer[offset..offset + 16];
                    (
                        i32::from_ne_bytes([b[0], b[1], b[2], b[3]]),
                        u32::from_ne_bytes([b[4], b[5], b[6], b[7]]),
                        u32::from_ne_bytes([b[8], b[9], b[10], b[11]]),
                        u32::from_ne_bytes([b[12], b[13], b[14], b[15]]) as usize,
                    )
                };
                let name_len = len.min(buffer.len() - offset - 16);
                let named = name_len != 0
                    && buffer[offset + 16..offset + 16 + name_len]
                        .split(|byte| *byte == 0)
                        .next()
                        .is_some_and(|name| !name.is_empty());
                offset += 16 + len;
                if mask & IN_Q_OVERFLOW != 0 {
                    refused_vnode_overflow();
                    continue;
                }
                if mask & IN_UNMOUNT != 0 {
                    refused_vnode_unmount();
                    continue;
                }
                let Some(watch) = self
                    .lock()
                    .get(&wd)
                    .and_then(Weak::upgrade)
                    .filter(|watch| !woken.iter().any(|seen| Arc::ptr_eq(seen, watch)))
                else {
                    continue;
                };
                if watch.note(mask, named) {
                    if !woken.iter().any(|seen| Arc::ptr_eq(seen, &watch)) {
                        woken.push(watch);
                    }
                    fired += 1;
                }
            }
        }
        fired
    }
}

/// One `EVFILT_VNODE` registration: an `inotify` watch, and the note word it has
/// accumulated.
///
/// **`ident` is a descriptor, and the descriptor is not what is watched.** The
/// guest's `open(path, O_EVTONLY)` is answered on this host with `O_PATH`
/// ([`crate::fcntl`], and the reason it is the same capability), and
/// `inotify_add_watch` takes a *pathname*, not a descriptor - measured: it
/// answers `EFAULT` for an `O_PATH` descriptor, because the second argument is a
/// `const char *`. So the path is recovered from `/proc/self/fd/<ident>` and the
/// object it names is checked against the object the descriptor holds, twice.
/// That check is the whole of the difference between watching what the caller
/// opened and watching whatever is at that name later, and XNU has no such
/// window because it attaches the knote to the `vnode` the descriptor holds.
pub struct Vnode {
    /// The kqueue's descriptor, remembered only so the wait can name it.
    queue: c_int,
    /// `ident` as the descriptor it is.
    ///
    /// **This is what [`Vnode::attrib_note`] asks about, and not `path`.** A knote
    /// in XNU is attached to the *vnode*, and the descriptor holds that vnode;
    /// the path is only a name for it, and the name can be given to a different
    /// object. It is also the only one of the two that still answers after an
    /// `unlink` - measured, an `unlink` of a watched file leaves `fstat` on the
    /// still-open `O_PATH` descriptor reporting `nlink = 0` while `stat` on the
    /// path is already `ENOENT`, so a link-count drop on a file with no names
    /// left is invisible to anything that stats the path.
    fd: c_int,
    /// The `inotify` instance this registration's watch lives in.
    inotify: Arc<Inotify>,
    /// The `inotify` watch descriptor, from `inotify_add_watch`.
    wd: c_int,
    /// The path recovered from `/proc/self/fd/<ident>`, and the only thing a
    /// later `stat` can be asked about.
    path: Vec<u8>,
    /// `(st_dev, st_ino)` of the object the descriptor held at registration,
    /// checked before and after `inotify_add_watch`.
    identity: (u64, u64),
    /// The link count at registration and after the last `IN_ATTRIB`. Zero means
    /// "unknown", and it is only ever reached by a `stat` that failed.
    nlink: Mutex<u64>,
    /// Whether a `NOTE_DELETE` has already been reported for this watch.
    ///
    /// **It exists because the two events that mean "this vnode lost a name" are
    /// both real, in that order, and reporting the fact twice is a busy watcher.**
    /// Measured: an `unlink` of a watched file delivers `IN_ATTRIB` while the
    /// descriptor is open, and `IN_DELETE_SELF` when it is closed - and
    /// `FilePathWatcherKQueue::HandleDeleteOrMoveChange` closes the descriptor as
    /// soon as it has seen the first one, so on this guest the second event is
    /// guaranteed to arrive. XNU posts `NOTE_DELETE` once, from `VNOP_REMOVE`.
    delete_reported: std::sync::atomic::AtomicBool,
    /// The notes that have fired since the last delivery. `EV_CLEAR` empties it;
    /// XNU's `knote_fill_kevent_with_sdata` empties `kn_fflags` on exactly the
    /// same condition, and this is that.
    pending: Mutex<u32>,
    /// The `fflags` after the narrowing, i.e. the `kn_sfflags` this knote has.
    mask: u32,
    /// The caller's own `ident`, `udata` and narrowed `fflags`, echoed in a
    /// delivered event exactly as XNU echoes them.
    ident: u64,
    udata: u64,
}

impl Drop for Vnode {
    fn drop(&mut self) {
        self.inotify.remove(self.wd);
    }
}

impl Vnode {
    /// Watch the object the descriptor `ident` holds, for the notes in `mask`.
    ///
    /// `inotify` is this kqueue's instance; the watch is added to it, so it is
    /// the descriptor that has to be waited on, and it is
    /// [`Inotify::raw_fd`] rather than `ident` that goes into the readiness set.
    pub fn watch(
        queue: c_int,
        inotify: Arc<Inotify>,
        ident: u64,
        mask: u32,
        udata: u64,
    ) -> Result<Arc<Self>, i32> {
        let Some(fd) = c_int::try_from(ident).ok() else {
            refused_vnode_ident(ident);
            return Err(crate::errno::from_host(libc::EBADF));
        };
        // `EV_ADD` names a descriptor, and one that is not open is `EBADF` at
        // registration rather than at the first wait - which is where BSD reports
        // it, and the only place a caller looks.
        let before = match stat_of(fd) {
            Some(stat) => stat,
            None => {
                let errno = crate::errno::get();
                refused_vnode_ident(ident);
                return Err(crate::errno::to_host(errno));
            }
        };
        let path = path_of(fd)?;
        if inotify
            .lock()
            .values()
            .filter_map(Weak::upgrade)
            .any(|watch| watch.identity == (before.dev, before.ino) && watch.ident == ident)
        {
            refused_vnode_twice(ident, &path);
            return Err(crate::errno::from_host(libc::EEXIST));
        }
        if stat_path(&path).map(|now| (now.dev, now.ino)) != Some((before.dev, before.ino)) {
            refused_vnode_identity(ident, &path);
            return Err(crate::errno::from_host(libc::EINVAL));
        }
        let wd = {
            let Ok(name) = std::ffi::CString::new(path.clone()) else {
                return Err(crate::errno::from_host(libc::EINVAL));
            };
            // SAFETY: `name` is a NUL-terminated path with no interior NUL, and
            // the pointer is valid for the length of the call.
            let wd = unsafe {
                libc::syscall(
                    libc::SYS_inotify_add_watch,
                    inotify.raw_fd(),
                    name.as_ptr(),
                    VNODE_INOTIFY_MASK,
                )
            } as c_int;
            if wd < 0 {
                let errno = crate::errno::get();
                refused_vnode_watch(ident, &path, errno);
                return Err(crate::errno::to_host(errno));
            }
            wd
        };
        // The second half of the identity check, and the reason it is done twice:
        // a path can be replaced between the `stat` and the `inotify_add_watch`,
        // and a watch on the wrong inode reports changes for a file the caller
        // never opened.
        if stat_of(fd).map(|now| (now.dev, now.ino)) != Some((before.dev, before.ino)) {
            inotify.remove(wd);
            refused_vnode_identity(ident, &path);
            return Err(crate::errno::from_host(libc::EINVAL));
        }
        let watch = Arc::new(Self {
            queue,
            fd,
            wd,
            path,
            identity: (before.dev, before.ino),
            nlink: Mutex::new(before.nlink),
            delete_reported: std::sync::atomic::AtomicBool::new(false),
            pending: Mutex::new(0),
            mask,
            ident,
            udata,
            inotify: Arc::clone(&inotify),
        });
        inotify.insert(&watch);
        Ok(watch)
    }

    /// The `inotify` instance this registration is watched through, which is
    /// what the kqueue waits on and is **not** `ident`.
    pub fn inotify_fd(&self) -> c_int {
        self.inotify.raw_fd()
    }

    /// A second registration on the same queue must join the *same* instance, or
    /// the queue would have two `inotify` descriptors and a `drain` on one would
    /// never see the other's events.
    pub fn shared_inotify(&self) -> Arc<Inotify> {
        Arc::clone(&self.inotify)
    }

    /// The kqueue this registration belongs to, for a trace line.
    pub fn queue(&self) -> c_int {
        self.queue
    }

    /// The caller's `ident`, echoed in a delivered event.
    pub fn ident(&self) -> usize {
        self.ident as usize
    }

    /// The caller's `udata`, echoed in a delivered event.
    pub fn udata(&self) -> usize {
        self.udata as usize
    }

    /// The `fflags` this knote is interested in, after the narrowing.
    pub fn mask(&self) -> u32 {
        self.mask
    }

    /// Fold one `inotify` event into the pending note word. Returns whether
    /// anything was added.
    fn note(&self, mask: u32, named: bool) -> bool {
        let translated = self.translate(mask, named);
        if translated == 0 {
            return false;
        }
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        // A delivered word never carries a bit outside the caller's interest:
        // this is the `kn_sfflags` intersection `filt_vnode_common` performs
        // with `if (kn->kn_sfflags & hint)`.
        let gained = translated & self.mask & !*pending;
        *pending |= translated & self.mask;
        gained != 0
    }

    /// The whole of the note-to-event mapping, one row at a time.
    ///
    /// **`named` is the distinction that carries the argument.** A name is
    /// present exactly when the event is about an entry *inside* a watched
    /// directory, and absent when it is about the watched object itself, and
    /// those two cases are two different translations rather than one.
    fn translate(&self, mask: u32, named: bool) -> u32 {
        if named {
            // The directory's contents changed. XNU raises `NOTE_WRITE` on the
            // *directory* for every one of these, and each row below is the XNU
            // call site that says so:
            //
            //   inotify      XNU (`bsd/vfs/kpi_vfs.c`)                        note
            //   IN_CREATE    VNOP_MKNOD:  `post_event_if_success(dvp, NOTE_WRITE)`  NOTE_WRITE
            //   IN_DELETE    VNOP_REMOVE: `post_event_if_success(dvp, NOTE_WRITE)`  NOTE_WRITE
            //   IN_MOVED_*   VNOP_RENAME: `int events = NOTE_WRITE; ... dvp`       NOTE_WRITE
            //
            // `IN_ATTRIB` with a name is **not** in that table and must not be
            // added to it: it is a *child's* metadata change, and XNU raises it
            // on the child's own vnode (`VNOP_SETXATTR`:
            // `post_event_if_success(vp, error, NOTE_ATTRIB)`), where this
            // registration has no knote. Putting `NOTE_ATTRIB` on the parent
            // would claim the parent's metadata changed, which is a claim about
            // a different file - so nothing is reported for it, and the fact is
            // reported by the child's own watch, which is where XNU puts it.
            if mask & (IN_CREATE | IN_DELETE | IN_MOVED_FROM | IN_MOVED_TO) != 0 {
                return NOTE_WRITE;
            }
            return 0;
        }
        let mut note = 0;
        if mask & IN_MODIFY != 0 {
            // `write(2)` and `truncate(2)` on the object. XNU:
            // `lock_vnode_and_post(pvp, NOTE_WRITE)` in `vfs_syscalls.c`, and
            // `VNOP_SETATTR` with `va_flags` set. `NOTE_EXTEND` is *not* set
            // here even though XNU may set it too, because `IN_MODIFY` does not
            // say the size grew - see `VNODE_UNREPORTABLE`.
            note |= NOTE_WRITE;
        }
        if mask & IN_DELETE_SELF != 0
            && !self
                .delete_reported
                .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            // The object has no names left. XNU `VNOP_REMOVE`:
            // `post_event_if_success(vp, _err, NOTE_DELETE | NOTE_LINK)` - of which
            // only the `NOTE_DELETE` half is reportable here, and which XNU posts
            // **once** even though the vnode goes on to be recycled. So the
            // `IN_ATTRIB` that already said so is not said again here; see
            // `delete_reported`.
            note |= NOTE_DELETE;
        }
        if mask & IN_MOVE_SELF != 0 {
            // The object was renamed. XNU `VNOP_RENAME`:
            // `lock_vnode_and_post(fvp, NOTE_RENAME)`, on the renamed-from
            // vnode, with `NOTE_DELETE` on the renamed-to one.
            note |= NOTE_RENAME;
        }
        if mask & IN_IGNORED != 0 {
            // The kernel dropped the watch because the object is gone, which is
            // XNU's `vnode_recycle`: `KNOTE(&vp->v_knotes, NOTE_REVOKE)`. The
            // two reasons a watch goes - the object went, or the caller removed
            // it - are told apart by whether the `wd` is still in the map; see
            // `Inotify::remove`.
            note |= NOTE_REVOKE;
        }
        if mask & IN_ATTRIB != 0 {
            note |= self.attrib_note();
        }
        note
    }

    /// `IN_ATTRIB` is one bit for three facts, so this decides which it is.
    ///
    /// **XNU has three notes here and `inotify` has one event**, and only two of
    /// them are a narrowing of the others:
    ///
    /// | fact | XNU | `inotify` |
    /// |---|---|---|
    /// | a name for this vnode was removed | `VNOP_REMOVE`: `NOTE_DELETE \| NOTE_LINK` | `IN_ATTRIB` |
    /// | a name was added (a hard link) | `VNOP_LINK`: `NOTE_LINK` | `IN_ATTRIB` |
    /// | mode, ownership, xattrs, timestamps | `VNOP_SETATTR`: `NOTE_ATTRIB` | `IN_ATTRIB` |
    ///
    /// The link count is therefore read and compared with the value from the last
    /// time it was read, which is what separates all three:
    ///
    /// * **down** - a name for this vnode was removed, and `NOTE_DELETE` is
    ///   reported. This is XNU's own `NOTE_DELETE`, raised by `VNOP_REMOVE` on the
    ///   *vnode*, whichever of its names was unlinked - and it is the case that
    ///   matters here, because it is the **only** one of the three that
    ///   `IN_DELETE_SELF` does not also carry: measured, an `unlink` of a watched
    ///   file with its `O_PATH` descriptor still open delivers **`IN_ATTRIB` and
    ///   nothing else**, and `IN_DELETE_SELF` and `IN_IGNORED` arrive only when
    ///   the last reference to the inode goes, which for `FilePathWatcherKQueue`
    ///   is after `HandleDeleteOrMoveChange` has closed the descriptor. So without
    ///   this the framework would learn about a deleted file on the *second*
    ///   collection rather than the first.
    /// * **up** - a hard link, and **nothing** is reported, because the note is
    ///   `NOTE_LINK` and `NOTE_LINK` is refused by name. Reporting `NOTE_ATTRIB`
    ///   would be a claim macOS does not make: `VNOP_LINK` posts
    ///   `post_event_if_success(vp, _err, NOTE_LINK)` and nothing else.
    /// * **unchanged** - `NOTE_ATTRIB`, which is what XNU posts for a mode,
    ///   ownership or xattr change.
    /// * **the count cannot be read** - nothing is reported, and the remembered
    ///   count is set to zero, which is a value no live object can have and
    ///   therefore makes every later `IN_ATTRIB` on this registration silent
    ///   rather than wrong.
    ///
    /// **The count is read with `fstat` on the descriptor, and not with `stat` on
    /// the path.** Two reasons, and the second is the one that was a bug:
    ///
    /// * A knote in XNU is attached to the vnode and the descriptor holds that
    ///   vnode, while the path is a *name* that can be handed to a different
    ///   object - which is the window [`Vnode::watch`] checks twice for and would
    ///   then quietly walk into here.
    /// * After an `unlink` the path does not exist, so `stat` is `ENOENT` and the
    ///   link count is unreadable at exactly the moment it is the only thing that
    ///   distinguishes a delete from a metadata change. Measured: `fstat` on the
    ///   open `O_PATH` descriptor reports `nlink = 0` where `stat` on the path
    ///   reports `ENOENT`.
    ///
    /// The path is still the fallback, for a registration whose descriptor is no
    /// longer open - which is silent rather than wrong, per the last bullet above.
    ///
    /// **The one imprecision, stated rather than hidden.** A `write` updates the
    /// modification time and a `truncate` is a `VNOP_SETATTR`, and either can come
    /// with an `IN_ATTRIB` that is a real metadata change, which *is* reported. If
    /// this host raised `IN_ATTRIB` for a timestamp alone, where XNU posts only
    /// `NOTE_WRITE` from `write(2)`'s `lock_vnode_and_post(pvp, NOTE_WRITE)`, that
    /// would be a *superset* by one bit on an event that already carries
    /// `NOTE_WRITE`. **Measured on this host, it does not**: a plain append
    /// delivers `IN_MODIFY` alone, and a `truncate` delivers `IN_MODIFY` alone.
    /// So there is no superset here to bound, and the imprecision is recorded as
    /// absent rather than argued about. It would still be named in
    /// [`announce_vnode_delivered`] if it ever appeared.
    fn attrib_note(&self) -> u32 {
        let mut remembered = self.nlink.lock().unwrap_or_else(|e| e.into_inner());
        let note = match stat_of(self.fd).or_else(|| stat_path(&self.path)) {
            Some(now) if now.nlink < *remembered => {
                *remembered = now.nlink;
                NOTE_DELETE
            }
            Some(now) if now.nlink > *remembered => {
                *remembered = now.nlink;
                0
            }
            Some(_) => NOTE_ATTRIB,
            None => {
                *remembered = 0;
                0
            }
        };
        if note & NOTE_DELETE != 0 {
            self.delete_reported.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        note
    }

    /// The notes waiting to be delivered, and whether to leave them waiting.
    ///
    /// `clear` is `EV_CLEAR`, and it empties the word after the caller has read
    /// it - `knote_fill_kevent_with_sdata` clears `kn_fflags` at the same point
    /// in the same order, after the copy-out has been prepared.
    pub fn take_pending(&self, clear: bool) -> u32 {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let word = *pending;
        if clear {
            *pending = 0;
        }
        word
    }

    /// The `flags` a delivered event carries.
    ///
    /// **The caller's own word, plus the kernel's additions, and the additions
    /// are the reference's.** `<sys/event.h>`'s `EV_RECEIPT` line and XNU's
    /// `kevent_register` together make the caller's `EV_ADD` part of the word
    /// already - the same argument as `PROC_DELIVERED_FLAGS`, for the same
    /// reason. And `filt_vnode_common` adds two bits of its own on a
    /// `NOTE_REVOKE`:
    ///
    /// ```c
    /// if (NOTE_REVOKE == hint) {
    ///         kn->kn_flags |= (EV_EOF | EV_ONESHOT);
    /// ```
    ///
    /// so a `NOTE_REVOKE` event is the last one this knote gives: the framework
    /// reads neither `flags` nor `data` off a vnode event, but a caller that did
    /// is owed the truth, and the truth is that the object is at end of file and
    /// the knote is one-shot.
    pub fn delivered_flags(&self, note: u32) -> u16 {
        let mut flags = EV_ADD;
        if note & NOTE_REVOKE != 0 {
            flags |= EV_EOF | EV_ONESHOT;
        }
        flags
    }
}

/// What one `stat` was asked for.
struct Stat {
    dev: u64,
    ino: u64,
    nlink: u64,
}

fn stat_of(fd: c_int) -> Option<Stat> {
    // SAFETY: `fd` is a descriptor the caller holds, and `stat` fills a
    // `struct stat` this module owns.
    let mut out: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &raw mut out) } < 0 {
        return None;
    }
    Some(Stat {
        dev: out.st_dev as u64,
        ino: out.st_ino as u64,
        nlink: out.st_nlink as u64,
    })
}

fn stat_path(path: &[u8]) -> Option<Stat> {
    let mut name = path.to_vec();
    name.push(0);
    let mut out: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `name` is a NUL-terminated path, and the pointer is valid for the
    // length of the call. `AT_SYMLINK_NOFOLLOW` so the answer is about the name
    // that was recovered from the descriptor and not about a symlink's target,
    // matching the `fstat` it is compared against.
    let rc = unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            name.as_ptr().cast::<libc::c_char>(),
            &raw mut out,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        return None;
    }
    Some(Stat {
        dev: out.st_dev as u64,
        ino: out.st_ino as u64,
        nlink: out.st_nlink as u64,
    })
}

/// The path the descriptor `fd` was opened with, from `/proc/self/fd/<fd>`.
fn path_of(fd: c_int) -> Result<Vec<u8>, i32> {
    let mut link = format!("/proc/self/fd/{fd}").into_bytes();
    link.push(0);
    let mut buf = vec![0u8; 4096];
    // SAFETY: `link` is NUL-terminated, `buf` is 4096 writable bytes, and the
    // return value is clamped to the buffer's length, which is what `readlink`
    // does with a short path.
    let n = unsafe {
        libc::readlink(
            link.as_ptr().cast::<libc::c_char>(),
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len() - 1,
        )
    };
    if n < 0 {
        let errno = crate::errno::get();
        refused_vnode_path(fd, errno);
        return Err(crate::errno::to_host(errno));
    }
    buf.truncate(n as usize);
    Ok(buf)
}

/// The names of the `EVFILT_VNODE` notes, for a message that has to name one.
fn note_names(notes: u32) -> String {
    const NAMES: [(u32, &str); 11] = [
        (NOTE_DELETE, "NOTE_DELETE"),
        (NOTE_WRITE, "NOTE_WRITE"),
        (NOTE_EXTEND, "NOTE_EXTEND"),
        (NOTE_ATTRIB, "NOTE_ATTRIB"),
        (NOTE_LINK, "NOTE_LINK"),
        (NOTE_RENAME, "NOTE_RENAME"),
        (NOTE_REVOKE, "NOTE_REVOKE"),
        (NOTE_NONE, "NOTE_NONE"),
        (NOTE_FUNLOCK, "NOTE_FUNLOCK"),
        (NOTE_LEASE_DOWNGRADE, "NOTE_LEASE_DOWNGRADE"),
        (NOTE_LEASE_RELEASE, "NOTE_LEASE_RELEASE"),
    ];
    let named: Vec<&str> = NAMES
        .iter()
        .filter(|(bit, _)| notes & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    if named.is_empty() {
        return format!("{notes:#x}, which is not a NOTE_ this filter has");
    }
    format!("{notes:#x} ({})", named.join(" | "))
}

/// **The message this filter is about.** A registration asked for notes that this
/// host's `inotify` cannot distinguish, and the registration is carrying on
/// without them - which is only acceptable because each dropped note is named
/// here and because the caller was told *which* notes it is no longer being told
/// about.
///
/// **It is a refusal of each dropped note individually, and the first occurrence
/// of each note is what prints.** The interesting failure is a run in which the
/// watcher works and nobody can tell which notes it is blind to; a browser that
/// watches a hundred paths says it once per note and then stops.
pub fn announce_vnode_note_dropped(ident: u64, asked: u32, dropped: u32) {
    use std::sync::OnceLock;
    static SEEN: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
    let mut seen = SEEN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let first: Vec<u32> = (0..32)
        .map(|bit| 1u32 << bit)
        .filter(|bit| dropped & bit != 0 && seen.insert(*bit))
        .collect();
    if first.is_empty() {
        return;
    }
    for bit in first {
        let reason = match bit {
            NOTE_EXTEND => "NOTE_EXTEND is \"size increased\", which is a narrowing of \
                NOTE_WRITE rather than a different fact, and inotify's IN_MODIFY is one bit for \
                both: it does not say the file grew. Reporting it would claim a size change that \
                may not have happened. NOTE_WRITE is reported instead, with the scope XNU gives \
                it",
            NOTE_LINK => "NOTE_LINK is \"link count changed\", and inotify folds a link-count \
                change into the same IN_ATTRIB it uses for a mode, ownership, timestamp or xattr \
                change. That is not a narrowing - the two notes are different facts - so the link \
                count is read with a stat and compared, and a change in it produces no note at \
                all rather than the wrong one",
            NOTE_NONE => "NOTE_NONE is <sys/event.h>'s own \"No specific vnode event\" bit, \
                which asks a filter to activate without saying what happened, and there is \
                nothing to report under it",
            NOTE_FUNLOCK => "NOTE_FUNLOCK is \"vnode was unlocked by flock(2)\", and flock is \
                advisory: an unlock is not a filesystem change and raises no filesystem event",
            NOTE_LEASE_DOWNGRADE | NOTE_LEASE_RELEASE => "the lease notes are requests XNU \
                answers by waking a lease holder, and there is no filesystem event behind one \
                on this host",
            _ => "this host has no inotify event that says the same thing",
        };
        eprintln!(
            "rosetta3: kevent: refused {} of the EVFILT_VNODE fflags {} on ident {ident:#x} - \
             {reason}. The registration is carried on for the {} it can report, and a refused \
             note is never set in a delivered fflags, so nothing is reported that was not observed",
            note_names(bit),
            note_names(asked),
            note_names(asked & VNODE_REPORTABLE),
        );
    }
}

/// A registration that asked for **nothing this implementation can report**.
///
/// **Refused rather than accepted**, because a registration that succeeds and can
/// never fire is the one shape of answer this project calls a lie: the caller
/// holds a subscription, the queue reports the descriptor readable, and no event
/// ever arrives. `EINVAL`, named, and the caller is told exactly which notes it
/// asked for and which of them are the problem.
pub fn refused_vnode_mask(ident: u64, fflags: u32) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent refused an EVFILT_VNODE on ident {ident:#x} with fflags {}: every \
             one of them is a note this host cannot observe, so there is no filter left to \
             register. The reportable notes are {} - answered on a real inotify watch, and \
             refused for a change is refused by name rather than mapped to the nearest event. \
             Accepting this would answer a subscription that could never fire",
            note_names(fflags),
            note_names(VNODE_REPORTABLE),
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// A `NOTE_` that is not an `EVFILT_VNODE` note at all.
///
/// `ENOTSUP`, which is the number XNU's own `filt_procattach` gives for a `NOTE_`
/// it does not implement, so a caller can tell "this filter has no such note"
/// apart from "this note is narrower than you asked for".
fn refused_vnode_note(ident: u64, fflags: u32) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent refused fflags {fflags:#x} on an EVFILT_VNODE with ident {ident:#x}: \
             the bits outside {VNODE_NOTES:#x} are not EVFILT_VNODE notes. The vnode notes are {} \
             and the two among them this host cannot observe are {} - refused by name, never \
             answered with the nearest event",
            note_names(VNODE_NOTES),
            note_names(VNODE_UNREPORTABLE),
        );
    });
    crate::errno::from_host(libc::ENOTSUP)
}

/// An action bit an `EVFILT_VNODE` has no meaning for.
fn refused_vnode_action(flags: u16, ident: u64) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent refused flags {flags:#x} on an EVFILT_VNODE with ident {ident:#x}: \
             the bits outside {VNODE_SERVED:#x} are EV_EOF ({EV_EOF:#x}) and EV_ERROR \
             ({EV_ERROR:#x}), which the kernel returns rather than accepts, and EV_SYSFLAGS, which \
             it reserves. The registration the framework makes - EV_ADD|EV_CLEAR|EV_RECEIPT with \
             all seven vnode notes - is answered"
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// An `ident` that is not a descriptor this host has open.
fn refused_vnode_ident(ident: u64) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent refused an EVFILT_VNODE on ident {ident:#x}: it is not a descriptor \
             this host has open, so there is no object to watch. Darwin reports EBADF for the same \
             change, at registration rather than at the first wait, and a knote attached to nothing \
             could never fire"
        );
    });
}

/// A descriptor whose path `/proc/self/fd` will not give back.
fn refused_vnode_path(fd: c_int, errno: i32) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent refused an EVFILT_VNODE on descriptor {fd}: readlink of \
             /proc/self/fd/{fd} failed with {errno:#x}, and inotify_add_watch takes a pathname \
             rather than a descriptor - measured, it answers EFAULT for an O_PATH descriptor \
             because the second argument is a const char *. Without the path there is nothing to \
             watch, and the alternative - watching the descriptor number - is a different filter"
        );
    });
}

/// Two registrations on one queue for the same object.
///
/// **Refused rather than shared**, and the reason is specific: one `inotify`
/// watch descriptor per object per instance, so a second registration would either
/// clobber the first or need its own instance, and a clobbered first registration
/// would stop reporting changes for a path its caller is still watching. `EEXIST`,
/// named. No registration in the framework does this - `EventsForPath` opens each
/// component once - so the refusal costs nothing here and is a capability named
/// rather than a silent loss of one of the two.
fn refused_vnode_twice(ident: u64, path: &[u8]) {
    eprintln!(
        "rosetta3: kevent refused a second EVFILT_VNODE on ident {ident:#x} for {}: this \
         kqueue already watches that object, and one inotify watch descriptor per object per \
         instance means a second registration would either replace the first - leaving a caller \
         with a subscription that has stopped reporting - or need a second instance. The \
         framework registers each path component once per watcher, so nothing in it does this",
        String::from_utf8_lossy(path),
    );
}

/// A path that no longer names the object the descriptor holds.
///
/// **This is the race XNU does not have**, and it is the reason the check is made
/// twice. `inotify_add_watch` takes a path, so the object is named twice - once by
/// the `open` the caller did, and once by the `readlink` of `/proc/self/fd` - and
/// between them the name can be pointed at a different inode. A watch on the
/// wrong inode reports changes for a file nobody opened, which is the wrong
/// answer this project exists to refuse, so the name is `stat`ed and compared with
/// the descriptor's own `fstat` before the watch is added and again after.
fn refused_vnode_identity(ident: u64, path: &[u8]) {
    eprintln!(
        "rosetta3: kevent refused an EVFILT_VNODE on ident {ident:#x}: {} does not name the \
         object that descriptor holds. inotify_add_watch takes a pathname, so the object was named \
         twice - by the caller's open and by the readlink of /proc/self/fd - and the name moved \
         between them. A watch on the other inode would report changes for a file the caller never \
         opened, so this is refused rather than watched",
        String::from_utf8_lossy(path),
    );
}

/// `inotify_add_watch` refused the path itself.
fn refused_vnode_watch(ident: u64, path: &[u8], errno: i32) {
    eprintln!(
        "rosetta3: kevent refused an EVFILT_VNODE on ident {ident:#x} for {}: inotify_add_watch \
         answered {errno:#x} with mask {VNODE_INOTIFY_MASK:#x}. Darwin's own reason for refusing \
         a vnode knote is ENOTSUP (filt_vnode_common: \"if (kn->kn_vnode_use != VIL_FSTATS) \
         knote_set_error(kn, ENOTSUP)\"), and the host's number is passed through rather than \
         replaced, so the guest sees the failure this host actually had",
        String::from_utf8_lossy(path),
    );
}

/// The `inotify` queue overflowed, so events were lost.
fn refused_vnode_overflow() {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent: inotify reported IN_Q_OVERFLOW, so file system events were lost \
             before they could be read. The notes that did arrive are still reported, because \
             they were observed, and the overflow itself is not reported as a note because there \
             is no vnode note for it - the alternative would be to attach a NOTE_WRITE or a \
             NOTE_ATTRIB to a file about something that happened to no file in particular. \
             XNU's kqueue has no equivalent loss: it never drops a knote's pending fflags"
        );
    });
}

/// The filesystem a watch was on went away.
fn refused_vnode_unmount() {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent: inotify reported IN_UNMOUNT, so the filesystem a watch was on is \
             gone. No vnode note says so - NOTE_REVOKE is a property of one vnode's access being \
             revoked, and every watch on the filesystem is affected at once - so nothing is reported \
             and this line says why the watcher has gone quiet"
        );
    });
}

/// `read` on the `inotify` descriptor failed in a way `EAGAIN` does not explain.
fn refused_vnode_read(errno: i32) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent: read on the inotify descriptor answered {errno:#x}, so this host \
             stopped handing over its own file system events. The notes already read are still \
             reported, because they were observed; nothing further will be, and a queue that is \
             waiting on this will have to be woken by something else"
        );
    });
}

/// Say that an `EVFILT_VNODE` registration was made, and exactly what it is.
///
/// **Off by default, and env-gated on `ROSETTA3_TRACE_EVFILT_VNODE`**, for the
/// reason `announce_proc_registered` is: the question this filter raises is
/// whether the *narrowed* mask is what the caller wanted, and the way to answer
/// it is a line per registration rather than an inference.
pub fn announce_vnode_registered(watch: &Vnode) {
    if std::env::var_os("ROSETTA3_TRACE_EVFILT_VNODE").is_none() {
        return;
    }
    eprintln!(
        "rosetta3: kqueue: EVFILT_VNODE registration for ident {:#x} (fd {}) on {}, udata {:#x}, \
         interest {} - an inotify watch on inode {} of the queue's own inotify instance {}. \
         XNU would attach a knote to the vnode the descriptor holds and set kn_sfflags to what \
         was asked for; this narrows it, and the dropped notes are named by \
         announce_vnode_note_dropped",
        watch.ident,
        watch.ident,
        String::from_utf8_lossy(&watch.path),
        watch.udata,
        note_names(watch.mask()),
        watch.identity.1,
        watch.inotify_fd(),
    );
}

/// Say what a delivered `EVFILT_VNODE` event says, in the reference's terms.
///
/// Off by default, env-gated on `ROSETTA3_TRACE_EVFILT_VNODE`, and **one line per
/// delivery**: this is the measurement that decides whether the filter is honest,
/// because a delivered `fflags` is the one value in it that can be wrong without
/// anything failing.
pub fn announce_vnode_delivered(watch: &Vnode, fflags: u32) {
    if std::env::var_os("ROSETTA3_TRACE_EVFILT_VNODE").is_none() {
        return;
    }
    eprintln!(
        "rosetta3: kqueue: delivered EVFILT_VNODE on ident {:#x} ({}) with fflags {} - {} - \
         flags {:#x}, data 0, udata {:#x}",
        watch.ident,
        String::from_utf8_lossy(&watch.path),
        fflags,
        note_names(fflags),
        watch.delivered_flags(fflags),
        watch.udata,
    );
}

/// An `EV_DELETE` of an `EVFILT_VNODE` that was never registered.
///
/// `pub` because [`crate::kqueue`] refuses the same `ENOENT` from its own table,
/// and the missing registration is the same thing in both.
pub fn announce_missing_vnode(ident: u64) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent: ENOENT for an EVFILT_VNODE with ident {ident:#x} - no such \
             registration on this queue, and so no inotify watch to remove. Darwin answers the \
             same way, and it is the check that catches a delete of a registration that was never \
             made"
        );
    });
}

/// The registrations of a queue whose descriptor has been closed.
///
/// **This is the one thing this module cannot do for itself**, and the reason is
/// a table it does not own: a queue's state is keyed by its descriptor number,
/// and nothing here is told when a descriptor is closed - `close` is
/// [`crate::net::descriptor`]'s, and that is the one place in this crate where a
/// per-descriptor table is already thrown away. So a queue's registrations
/// outlive its descriptor until its ports stop existing ([`watches`] drops
/// those), and a descriptor number handed to a *new* `kqueue()` inherits
/// whatever the last queue at that number had registered.
///
/// The effect is bounded and it is not a hang and not a wrong readiness: the
/// inherited knotes are only the ones whose ports are still alive, and
/// `WaitManyImpl` adds its own with `EV_ADD`, which replaces the identical keys.
/// A fresh queue can therefore be handed an event for a port its caller never
/// named, which is a lower `udata` than the one that really fired. Chromium
/// re-checks the event it is handed, so the run survives it, and Darwin cannot
/// do it at all.
///
/// Calling this from `close` - the one line beside
/// `super::sockopt::forget(fd)` in [`crate::net::descriptor`] - is the whole fix,
/// and until it is there this is what the tests below use to start from a known
/// table. That is also why they are deterministic at all.
pub fn forget_queue(queue: c_int) {
    queues()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&queue);
}

// Published by [`super`]'s `exports!` block, beside every other `mach_` and
// `vm_` name this crate answers for, rather than from here: a second block would
// make a `MODULE` that nothing includes, which is how a name ends up implemented
// and unbound.

#[cfg(test)]
mod tests {
    use super::*;

    /// A zero timeout, which makes a call a collection rather than a wait.
    static NO_WAIT: libc::timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };

    /// A one-second timeout, for a test that is waiting for something that
    /// should arrive long before that.
    static SECOND: libc::timespec = libc::timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };

    /// A fresh queue, with whatever the last queue at this descriptor number had
    /// registered thrown away first.
    ///
    /// See [`forget_queue`]: a test that closes a queue and opens another gets
    /// the same descriptor number back, and without this every test in this
    /// module would inherit the previous one's knotes.
    fn queue() -> c_int {
        let queue = crate::kqueue::kqueue();
        forget_queue(queue);
        queue
    }

    /// Chromium's `EV_SET(&change, pid, EVFILT_PROC, EV_ADD, NOTE_EXIT, 0, NULL)`,
    /// word for word. All three of the framework's registrations are these nine
    /// instructions; see the module note for the addresses.
    fn watch_proc(pid: i32, flags: u16) -> Kevent64 {
        Kevent64 {
            ident: pid as u32 as u64,
            filter: EVFILT_PROC,
            flags,
            fflags: NOTE_EXIT,
            data: 0,
            udata: 0,
            ext: [0, 0],
        }
    }

    /// Register one change, answering the `errno` it failed with.
    fn apply_one(queue: c_int, change: &Kevent64) -> Result<(), i32> {
        // SAFETY: one change, no events, no timeout.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::from_ref(change),
                1,
                std::ptr::null_mut(),
                0,
                0,
                std::ptr::null(),
            )
        };
        match n {
            0 => Ok(()),
            _ => Err(crate::errno::get()),
        }
    }

    /// Wait on a queue for up to a second, answering the events it reports.
    fn collect(queue: c_int, room: usize) -> Vec<Kevent64> {
        let mut got = vec![Kevent64::default(); room];
        // SAFETY: no changes, `room` event slots and a one-second timeout.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                room as c_int,
                0,
                &raw const SECOND,
            )
        };
        assert!(n >= 0, "kevent64 wait failed: {}", crate::errno::get());
        got.truncate(n as usize);
        got
    }

    /// This process's own pid, which is a pid that is certainly still running -
    /// the registration a test makes when it wants the change *rejected* before
    /// anything is opened, rather than a race against a child.
    fn self_pid() -> i32 {
        // SAFETY: `getpid` takes no arguments and cannot fail.
        unsafe { libc::getpid() }
    }

    /// A child that has already exited and been reaped, so its pid is free.
    ///
    /// A `pidfd` on a *zombie* is readable, and a `pidfd` on a pid that has
    /// been reaped and is gone cannot be opened at all - which is `ESRCH`, and
    /// which is the answer both of the framework's `EVFILT_PROC` callers treat
    /// as "it is dead". So a test needs a pid in each of those two states and
    /// this produces them: `exit_now` leaves a zombie this process has not
    /// reaped, and `reaped` leaves nothing at all.
    fn child_that_exits(reap: bool) -> i32 {
        // SAFETY: `fork` takes no arguments and both this and the child return.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed: {}", crate::errno::get());
        if pid == 0 {
            // SAFETY: `_exit` in the child, so no atexit handler and no
            // destructors run twice. `EXIT_SUCCESS` is 0 on this host.
            unsafe { libc::_exit(0) };
        }
        if reap {
            let mut status: c_int = 0;
            loop {
                // SAFETY: a live `pid_t` and a live out-parameter for the status.
                let done = unsafe { libc::waitpid(pid, &raw mut status, 0) };
                if done == pid {
                    break;
                }
                assert_eq!(
                    crate::errno::get(),
                    libc::EINTR,
                    "waitpid on our own child failed for a reason other than EINTR"
                );
            }
        }
        pid
    }

    fn receive_right() -> Name {
        let mut name = space::MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { space::mach_port_allocate(space::TASK_SELF, space::RIGHT_RECEIVE, &mut name) };
        name
    }

    fn port_set() -> Name {
        let mut name = space::MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { space::mach_port_allocate(space::TASK_SELF, space::RIGHT_PORT_SET, &mut name) };
        name
    }

    /// Chromium's `EV_SET64`: one event per waitable, the index in `udata`.
    fn add(port: Name, index: u64) -> Kevent64 {
        Kevent64 {
            ident: port as u64,
            filter: EVFILT_MACHPORT,
            flags: EV_ADD,
            fflags: 0,
            data: 0,
            udata: index,
            ext: [0, 0],
        }
    }

    /// An empty message to `port`, which is what a `WaitableEvent` signals with.
    fn signal(port: Name) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u32.to_le_bytes()); // msgh_bits
        bytes.extend_from_slice(&24u32.to_le_bytes()); // msgh_size
        bytes.extend_from_slice(&port.to_le_bytes());
        bytes.extend_from_slice(&space::MACH_PORT_NULL.to_le_bytes());
        bytes.extend_from_slice(&space::MACH_PORT_NULL.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        // SAFETY: a whole message of the size in its header.
        let rc = unsafe {
            super::super::msg::mach_msg(
                bytes.as_mut_ptr().cast(),
                0x1, // MACH_SEND_MSG
                bytes.len() as u32,
                0,
                space::MACH_PORT_NULL,
                0,
                space::MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, 0, "the signal reached the port");
    }

    /// `MessagePumpKqueue`'s registration, word for word.
    ///
    /// Read out of the framework at `Chromium Framework+0x5da59a8`, not out of
    /// the header: `MessagePumpKqueue::MessagePumpKqueue`,
    /// `base/message_loop/message_pump_kqueue.cc:146-155`, whose nine
    /// instructions are reproduced at the top of this file. `fflags` is
    /// `MACH_RCV_MSG`, `ext[0]` is `&wakeup_buffer_` and `ext[1]` is
    /// `sizeof(mach_msg_empty_rcv_t) == 32`.
    fn receive_during_delivery(port: Name, buffer: u64, length: u64) -> Kevent64 {
        Kevent64 {
            ident: port as u64,
            filter: EVFILT_MACHPORT,
            flags: EV_ADD,
            fflags: MACH_RCV_MSG,
            data: 0,
            udata: 0,
            ext: [buffer, length],
        }
    }

    /// A `mach_msg_empty_rcv_t`-shaped buffer: `sizeof` says 32, and the
    /// framework's own `mov QWORD PTR [rbp-0x150],0x20` agrees.
    fn receive_buffer() -> Vec<u8> {
        vec![0xa5; 32]
    }

    /// One `kevent64` call, with the changelist the guest would pass.
    fn call(queue: c_int, changes: &[Kevent64], room: usize) -> (i32, Vec<Kevent64>) {
        let mut got = vec![Kevent64::default(); room];
        // SAFETY: `changes` is `changes.len()` events and `got` has `room` slots.
        let n = unsafe {
            kevent64(
                queue,
                changes.as_ptr(),
                changes.len() as c_int,
                got.as_mut_ptr(),
                room as c_int,
                0,
                &raw const NO_WAIT,
            )
        };
        (n, got)
    }

    /// One `kevent64` call that will wait up to `timeout` for an event.
    ///
    /// [`call`] is a collection, which is the right shape for a port whose
    /// message is already there and the wrong shape for a deadline that has not
    /// passed yet - so this is the same call with a real timeout, and the tests
    /// that wait for a timer use it.
    fn wait_for(
        queue: c_int,
        changes: &[Kevent64],
        room: usize,
        timeout: &libc::timespec,
    ) -> (i32, Vec<Kevent64>) {
        let mut got = vec![Kevent64::default(); room];
        // SAFETY: `changes` is `changes.len()` events, `got` has `room` slots,
        // and `timeout` is a live `timespec`.
        let n = unsafe {
            kevent64(
                queue,
                changes.as_ptr(),
                changes.len() as c_int,
                got.as_mut_ptr(),
                room as c_int,
                0,
                std::ptr::from_ref(timeout),
            )
        };
        (n, got)
    }

    /// `ChangeOneEvent`: one change, no room for an event, no timeout. The
    /// shape `MessagePumpKqueue`'s constructor, `WatchMachReceivePort` and
    /// `StopWatchingMachPort` all use, and the one whose result
    /// `PCHECK(rv == 0) << "kevent64"` is a hard abort.
    fn change_one(queue: c_int, change: &Kevent64) -> i32 {
        // SAFETY: one readable change, and no event list to write.
        unsafe {
            kevent64(
                queue,
                std::ptr::from_ref(change),
                1,
                std::ptr::null_mut(),
                0,
                0,
                std::ptr::null(),
            )
        }
    }

    /// The `PCHECK(rv == 0)` at `message_pump_kqueue.cc:155`, verbatim.
    ///
    /// The framework's own constructor, so the struct here is the one read out
    /// of the binary at `Chromium Framework+0x5da59a8` and nothing else. This
    /// is the assertion the refusal used to fail, so it is the first thing that
    /// has to be true: a registration that will be honoured has to be accepted,
    /// because the guest's `CHECK_EQ` on the result leaves no other outcome
    /// that is not an abort.
    #[test]
    fn the_wakeup_registration_message_pump_kqueue_makes_is_accepted() {
        let queue = queue();
        let wakeup = receive_right();
        let mut buffer = receive_buffer();

        assert_eq!(
            change_one(queue, &receive_during_delivery(
                wakeup,
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
            )),
            0,
            "the registration at message_pump_kqueue.cc:155 is accepted"
        );
        // ... and a second one, because `MessagePumpKqueue` is constructed once
        // per `SequenceManager` thread and an `EV_ADD` must be idempotent.
        assert_eq!(
            change_one(queue, &receive_during_delivery(
                wakeup,
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
            )),
            0
        );

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(wakeup);
        }
    }

    /// **The lifecycle, and the part that makes it real: the receive takes the
    /// message, so the event goes quiet.**
    ///
    /// An implementation that accepted `MACH_RCV_MSG` and reported readiness
    /// without receiving would pass every test above and fail this one, and
    /// would fail it in the worst way: the guest would believe a message
    /// arrived, read a buffer with nothing in it, and go on believing it. So
    /// each of the four claims is checked against the port itself rather than
    /// against the event: the message is in the caller's buffer, the event says
    /// the receive succeeded, the port is no longer readable, and a second
    /// collection finds nothing.
    #[test]
    fn a_message_received_during_delivery_is_taken_and_the_event_goes_quiet() {
        let queue = queue();
        let wakeup = receive_right();
        let mut buffer = receive_buffer();
        assert_eq!(
            change_one(
                queue,
                &receive_during_delivery(
                    wakeup,
                    buffer.as_mut_ptr() as u64,
                    buffer.len() as u64,
                ),
            ),
            0
        );

        signal(wakeup);
        let (n, got) = call(queue, &[], 1);
        assert_eq!(n, 1, "the message woke the wait");
        assert_eq!(got[0].filter, EVFILT_MACHPORT);
        assert_eq!(got[0].ident, wakeup as u64, "ident is the registration's");
        assert_eq!(got[0].data, wakeup as i64, "data is the port that had it");
        assert_eq!(
            got[0].fflags as i32, super::super::kern::MACH_MSG_SUCCESS,
            "the delivered fflags is the mach_msg return code, and the receive worked"
        );
        assert_eq!(
            got[0].ext[1], 24,
            "ext[1] comes back as the received message's own msgh_size"
        );

        // The message is *in the buffer*, with the sentinel bytes around it
        // overwritten. `msgh_local_port` is the fourth u32 of the header and is
        // the field a received message carries the port's name in - the same
        // field `WaitableEvent::TimedWaitImpl` writes before its own receive
        // (`msg.header.msgh_local_port = receive_right_->Name();`,
        // `waitable_event_apple.cc:73`).
        let words: Vec<u32> = buffer
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
            .collect();
        assert_eq!(words[3], wakeup, "msgh_local_port: the port it came from");
        assert_eq!(words[1], 24, "msgh_size");
        assert_eq!(
            &buffer[24..],
            &[super::super::kern::MACH_MSG_SUCCESS.to_le_bytes()[0], 0, 0, 0, 8, 0, 0, 0],
            "the trailer is the 8-byte MACH_MSG_TRAILER_FORMAT_0, over the sentinel"
        );

        // **The clear.** The receive consumed the datagram, so the descriptor is
        // no longer readable and the same call finds nothing. This is a
        // property of the queue, not a flag: nothing in this file records "fired".
        assert_eq!(call(queue, &[], 1).0, 0, "and it does not fire again");
        assert_eq!(
            space::receiver(wakeup).map(|port| port.ready()),
            Some(false),
            "and the port really is quiet"
        );

        // A second message wakes it again, so the registration was not consumed
        // by the first - the guest re-arms nothing, and must not have to.
        signal(wakeup);
        assert_eq!(call(queue, &[], 1).0, 1, "a new message wakes it again");

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(wakeup);
        }
    }

    /// A receive that failed must say so in the event, and must not be
    /// reported as the success it was not.
    ///
    /// `<sys/event.h>` gives the delivered `fflags` a second meaning for this
    /// form - "the return code normally returned by `mach_msg()`" - precisely so
    /// that a caller can tell. A 16-byte buffer cannot hold a 24-byte message
    /// plus its trailer, so `mach_msg` answers `MACH_RCV_TOO_LARGE`, and that is
    /// what the event must carry. Reporting zero here would be a wrong answer
    /// that looks like success, which is the one outcome this file exists to
    /// avoid.
    #[test]
    fn a_receive_that_did_not_fit_is_reported_as_the_failure_it_was() {
        let queue = queue();
        let port = receive_right();
        let mut buffer = vec![0u8; 16];
        assert_eq!(
            change_one(
                queue,
                &receive_during_delivery(port, buffer.as_mut_ptr() as u64, buffer.len() as u64),
            ),
            0,
            "the buffer's length is not checked at registration, exactly as on Darwin"
        );

        signal(port);
        let (n, got) = call(queue, &[], 1);
        assert_eq!(n, 1, "the message did wake the wait");
        assert_eq!(
            got[0].fflags as i32,
            super::super::kern::MACH_RCV_TOO_LARGE,
            "the event carries the mach_msg failure rather than a success"
        );
        assert_ne!(got[0].fflags as i32, 0, "and that is not zero");
        assert_eq!(got[0].ext[1], 0, "nothing was received, so no size");

        // Without `MACH_RCV_LARGE` the too-large message is destroyed rather
        // than kept, so the port is quiet and the event does not lie by
        // re-firing on a message that is not on the queue any more.
        assert_eq!(call(queue, &[], 1).0, 0, "and the port is quiet after it");

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// A change-only call applies changes and reports nothing, so it must not
    /// receive: the registration happens in the constructor, long before any
    /// message, and a receive there would be a message taken out of a port with
    /// no event to account for it.
    #[test]
    fn a_change_only_call_registers_without_receiving() {
        let queue = queue();
        let port = receive_right();
        let mut buffer = receive_buffer();
        signal(port);
        assert_eq!(buffer[0], 0xa5, "the sentinel is still there before");

        // With a message already waiting, `ChangeOneEvent` still reports zero
        // events and must leave the message on the port.
        assert_eq!(
            change_one(
                queue,
                &receive_during_delivery(
                    port,
                    buffer.as_mut_ptr() as u64,
                    buffer.len() as u64,
                ),
            ),
            0
        );
        assert_eq!(buffer[0], 0xa5, "and the buffer is untouched");
        // The next collection is the one that receives, and it finds the
        // message that was there all along.
        let (n, got) = call(queue, &[], 1);
        assert_eq!(n, 1);
        assert_eq!(got[0].fflags as i32, super::super::kern::MACH_MSG_SUCCESS);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// `nevents` is a capacity, and a message may only be taken for an event
    /// that was actually handed over.
    ///
    /// The obvious implementation receives while it is still deciding which
    /// events fit, and then throws the extra ones away - which destroys a
    /// message and reports nothing, so the next wait on that port is silently
    /// one message short. Two ports with a message each and room for one is the
    /// smallest case that tells the two apart.
    #[test]
    fn a_message_is_only_taken_for_an_event_that_is_delivered() {
        let queue = queue();
        let first = receive_right();
        let second = receive_right();
        let mut one = receive_buffer();
        let mut two = receive_buffer();
        assert_eq!(
            call(
                queue,
                &[
                    receive_during_delivery(first, one.as_mut_ptr() as u64, one.len() as u64),
                    receive_during_delivery(second, two.as_mut_ptr() as u64, two.len() as u64),
                ],
                2,
            )
            .0,
            0
        );
        signal(first);
        signal(second);

        let (n, got) = call(queue, &[], 1);
        assert_eq!(n, 1, "only one fitted");
        assert_eq!(got[0].udata, 0, "and it is the lowest index");

        // The other message is still there: it was not received for an event
        // nobody was given.
        assert_ne!(one[0], 0xa5, "one holds a received message");
        assert_eq!(two[0], 0xa5, "two's buffer is untouched");
        assert_eq!(
            space::receiver(second).map(|port| port.ready()),
            Some(true),
            "and two's message is still on its port"
        );
        assert_eq!(call(queue, &[], 1).0, 1, "so it fires on the next turn");

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(first);
            space::destroy(second);
        }
    }

    /// The three registrations that are refused, each refused for a reason that
    /// is a property of this runtime and not of the header.
    #[test]
    fn receive_options_and_buffers_this_cannot_honour_are_refused_by_name() {
        let queue = queue();
        let port = receive_right();
        let set = port_set();
        let mut buffer = receive_buffer();

        // `MACH_RCV_LARGE`: the message is kept rather than destroyed, so the
        // port stops being readable while the message is still on it.
        let mut large = receive_during_delivery(port, buffer.as_mut_ptr() as u64, buffer.len() as u64);
        large.fflags = MACH_RCV_MSG | MACH_RCV_LARGE;
        assert_eq!(change_one(queue, &large), -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);

        // A zero length means the buffer comes from the `kevent_qos` output
        // area, which this call has none of.
        let mut no_buffer = receive_during_delivery(port, 0, 0);
        no_buffer.fflags = MACH_RCV_MSG;
        assert_eq!(change_one(queue, &no_buffer), -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);

        // A receive names one port; a set's event reports whichever member had
        // the message, and the `kevent64_s` does not say which.
        let mut on_a_set =
            receive_during_delivery(set, buffer.as_mut_ptr() as u64, buffer.len() as u64);
        on_a_set.fflags = MACH_RCV_MSG;
        assert_eq!(change_one(queue, &on_a_set), -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);

        // And the filter this file does answer is still answerable afterwards,
        // which is what makes each of the refusals above a statement about that
        // one registration rather than about the queue.
        assert_eq!(change_one(queue, &add(port, 0)), 0);
        signal(port);
        assert_eq!(call(queue, &[], 1).0, 1);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
            space::destroy(set);
        }
    }

    /// Take the message waiting on `port`, the way a `WaitManyImpl` caller would
    /// after being told there is one. A test that leaves it queued keeps being
    /// told there is one, which is correct and unhelpful.
    fn drain(port: Name) {
        let mut buffer = vec![0u8; 64];
        // SAFETY: 64 writable bytes, and the port is this test's own.
        let rc = unsafe {
            super::super::msg::mach_msg(
                buffer.as_mut_ptr().cast(),
                0x102, // MACH_RCV_MSG | MACH_RCV_TIMEOUT
                0,
                buffer.len() as u32,
                port,
                0,
                space::MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, 0, "the queued message was taken");
    }

    /// `MaybeUpdateWakeupTimer`'s two changes, word for word.
    ///
    /// Read out of the framework at `Chromium Framework+0x5da61a6` and
    /// `+0x5da6259` - the `EV_DELETE` and the `EV_ADD | EV_ONESHOT` -
    /// which are `SetWakeupTimerEvent` at `message_pump_kqueue.cc:357-382`.
    /// `PCHECK(rv == 0) << "kevent64, delete timer"` (line 572) and
    /// `<< "kevent64, set timer"` (line 580) are the same hard `CHECK_EQ` as
    /// the one at line 155, so an `EINVAL` here is a browser that aborts.
    fn set_timer(ident: u64, micros: i64) -> Kevent64 {
        Kevent64 {
            ident,
            filter: EVFILT_TIMER,
            flags: EV_ADD | EV_ONESHOT,
            fflags: NOTE_USECONDS,
            data: micros,
            udata: 0,
            ext: [0, 0],
        }
    }

    fn delete_timer(ident: u64) -> Kevent64 {
        Kevent64 {
            ident,
            filter: EVFILT_TIMER,
            flags: EV_DELETE,
            fflags: 0,
            data: 0,
            udata: 0,
            ext: [0, 0],
        }
    }

    /// A timer is armed, fires once, and is gone - and the deadline is the host
    /// kernel's, not a number this file compares.
    ///
    /// The proof that the fire is real and not a `sleep` is that the descriptor
    /// is *readable before the deadline* and not readable after it, which is
    /// only true of a `timerfd` somebody armed.
    #[test]
    fn a_timer_armed_with_a_deadline_fires_once_and_is_then_gone() {
        let queue = queue();
        // 300 ms is long enough that the two "not yet" observations below cannot
        // be a coincidence of timing, and short enough not to matter.
        assert_eq!(change_one(queue, &set_timer(0, 300_000)), 0);
        assert_eq!(
            call(queue, &[], 1).0,
            0,
            "not readable yet: the deadline has not passed"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(600);
        let (n, got) = wait_for(queue, &[], 1, &SECOND);
        assert_eq!(n, 1, "and then it is");
        assert!(
            std::time::Instant::now() < deadline,
            "and it did not fire early, which a comparison in a loop could not do"
        );
        assert_eq!(got[0].filter, EVFILT_TIMER);
        assert_eq!(got[0].ident, 0, "ident is the timer's own");
        assert_eq!(got[0].data, 0, "data is the time left, which is none");
        assert_eq!(got[0].fflags, NOTE_USECONDS, "fflags is the caller's unit");

        // `EV_ONESHOT`, and a one-shot `timerfd` that has expired is disarmed by
        // the kernel, so the next call finds nothing. The `poll` for this takes
        // its timeout, and a still-armed timer would come back here.
        assert_eq!(call(queue, &[], 1).0, 0, "a one-shot does not come back");
        assert_eq!(
            change_one(queue, &delete_timer(0)),
            -1,
            "and EV_DELETE of it is now ENOENT"
        );
        assert_eq!(crate::errno::get(), libc::ENOENT);

        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// Without `EV_ONESHOT` the timer keeps firing, which is what
    /// "All timeouts are implicitly EV_CLEAR events" means and what a `timerfd`
    /// armed with a non-zero `it_interval` does.
    #[test]
    fn a_timer_without_oneshot_keeps_firing() {
        let queue = queue();
        let mut repeating = set_timer(0, 1_000);
        repeating.flags = EV_ADD;
        assert_eq!(change_one(queue, &repeating), 0);

        for round in 1..=3 {
            let (n, got) = wait_for(queue, &[], 1, &SECOND);
            assert_eq!(n, 1, "round {round}: it fired again");
            assert_eq!(got[0].filter, EVFILT_TIMER);
        }
        assert_eq!(change_one(queue, &delete_timer(0)), 0, "and it can be deleted");

        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// `data < 0` is a deadline that has already passed: one immediate event and
    /// no repetition. XNU says so in `filt_timervalidate`, and says why - the old
    /// behaviour was a hot spin.
    #[test]
    fn a_negative_interval_fires_immediately_and_once() {
        let queue = queue();
        assert_eq!(change_one(queue, &set_timer(0, -5_000_000)), 0);
        let (n, got) = call(queue, &[], 1);
        assert_eq!(n, 1, "it is already due");
        assert_eq!(got[0].filter, EVFILT_TIMER);
        assert_eq!(
            call(queue, &[], 1).0,
            0,
            "once: the reference made this a one-shot precisely to stop the spin"
        );

        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// A timer and a port on one queue, and the `nevents` budget shared between
    /// them. The framework's pump is exactly this: the wakeup port, the Mach
    /// ports `WatchMachReceivePort` registered, and one timer, all on one queue
    /// with one `events_` vector.
    #[test]
    fn a_timer_and_a_port_share_one_queue_and_one_budget() {
        let queue = queue();
        let port = receive_right();
        call(queue, &[add(port, 0)], 2);
        assert_eq!(change_one(queue, &set_timer(0, 200_000)), 0);
        signal(port);

        // One wait returns what was ready when it woke, which here is the port:
        // the timer is 200 ms out. That is Darwin's own answer - a `kevent64`
        // does not linger for a second event - and the pump's loop is written
        // for it.
        let (n, got) = wait_for(queue, &[], 2, &SECOND);
        assert_eq!(n, 1, "the port, which was already there");
        assert_eq!(got[0].filter, EVFILT_MACHPORT);
        // The port is a plain registration, so its message is still queued and
        // the next wait reports the port again - level-triggered, and the reason
        // the delivered order walks the ports before the timers.
        assert_eq!(call(queue, &[], 2).0, 1);
        drain(port);

        let (n, got) = wait_for(queue, &[], 2, &SECOND);
        assert_eq!(n, 1, "and then the timer, once its deadline passes");
        assert_eq!(got[0].filter, EVFILT_TIMER);
        assert_eq!(got[0].ident, 0);

        // A second signal with one slot: the port is delivered and the timer's
        // event is not, and the timer is not consumed for it.
        assert_eq!(change_one(queue, &set_timer(0, 200_000)), 0);
        signal(port);
        let (n, got) = call(queue, &[], 1);
        assert_eq!(n, 1);
        assert_eq!(got[0].filter, EVFILT_MACHPORT);
        drain(port);
        assert_eq!(
            wait_for(queue, &[], 2, &SECOND).0,
            1,
            "and the timer, which was never delivered, is still there"
        );

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// The three timer forms this cannot answer, and the two it can that must not
    /// be confused with them.
    #[test]
    fn timer_registrations_this_cannot_answer_are_refused_by_name() {
        let queue = queue();

        // A calendar epoch: XNU freezes it at registration so an NTP step cannot
        // move it, and a CLOCK_REALTIME timerfd is moved by one.
        let mut absolute = set_timer(0, 1_000_000);
        absolute.fflags = NOTE_USECONDS | NOTE_ABSOLUTE;
        assert_eq!(change_one(queue, &absolute), -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);

        // Mach time. `CLOCK_BOOTTIME` is the nearest thing here and is not the
        // same clock, so it is refused rather than approximated.
        let mut mach = set_timer(0, 1_000_000);
        mach.fflags = NOTE_MACHTIME;
        assert_eq!(change_one(queue, &mach), -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);

        // Two unit bits at once matches no case of XNU's switch, which returns
        // EINVAL for exactly this.
        let mut two_units = set_timer(0, 1_000_000);
        two_units.fflags = NOTE_USECONDS | NOTE_NSECONDS;
        assert_eq!(change_one(queue, &two_units), -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);

        // The unit default - milliseconds - is not an error, and is answered.
        let mut millis = set_timer(0, 50);
        millis.fflags = 0;
        assert_eq!(change_one(queue, &millis), 0);
        let (n, _) = wait_for(queue, &[], 1, &SECOND);
        assert_eq!(n, 1, "50 ms of milliseconds fired");
        assert_eq!(
            change_one(queue, &delete_timer(0)),
            -1,
            "and it was a one-shot, so it is gone rather than re-armed"
        );
        assert_eq!(crate::errno::get(), libc::ENOENT);

        // And the form the framework uses, with leeway, is answered too.
        let mut leeway = set_timer(0, 1_000);
        leeway.fflags = NOTE_USECONDS | NOTE_LEEWAY;
        leeway.ext = [0, 500];
        assert_eq!(change_one(queue, &leeway), 0);
        assert_eq!(change_one(queue, &delete_timer(0)), 0);

        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// The positive control for everything below: the `pidfd` really opens on
    /// this host, and the number in [`SYS_pidfd_open`] is the right one.
    ///
    /// Without this, every other test here would pass just as well against a
    /// `pidfd_open` that returned `ENOSYS` and a filter that refused everything,
    /// and the suite would be green while the capability was absent.
    #[test]
    fn pidfd_open_of_this_process_works() {
        // SAFETY: `getpid` takes no arguments and cannot fail.
        let self_pid = unsafe { libc::getpid() };
        let fd = pidfd_open(self_pid as u32).expect("pidfd_open(getpid())");
        assert!(fd >= 0);

        // A `pidfd` on a process that is still running is **not** readable, and
        // saying otherwise would be the whole failure this file is written to
        // avoid - so this asserts the negative as well as the positive.
        let mut entry = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live `pollfd` and a zero timeout, which cannot block.
        let ready = unsafe { libc::poll(&raw mut entry, 1, 0) };
        assert_eq!(ready, 0, "a pidfd on a live process must not be readable");
        assert_eq!(entry.revents, 0);

        // SAFETY: this test's own descriptor.
        unsafe { libc::close(fd) };
    }

    /// The registration, the wait, and the three fields the framework reads.
    ///
    /// This is the shape of all three of the framework's `EVFILT_PROC` sites:
    /// `kevent(kq, &change, 1, NULL, 0, NULL)` and then
    /// `kevent(kq, NULL, 0, &event, 1, NULL)`.
    #[test]
    fn a_process_that_exits_reports_the_filter_fflags_and_ident_it_was_given() {
        let queue = queue();
        let pid = child_that_exits(/* reap */ false);
        assert_eq!(apply_one(queue, &watch_proc(pid, EV_ADD)), Ok(()));

        let got = collect(queue, 1);
        assert_eq!(got.len(), 1, "the child exited, so there is one event");
        // `mac_relauncher` reads exactly these three and logs the rest.
        assert_eq!(got[0].filter, EVFILT_PROC);
        assert_eq!(got[0].fflags, NOTE_EXIT);
        assert_eq!(got[0].ident, pid as u32 as u64);
        // The two fields no caller in the framework reads, reported as XNU sets
        // them: `filt_procattach` sets EV_CLEAR and `filt_procevent` sets
        // EV_EOF|EV_ONESHOT on NOTE_EXIT. In particular `EV_EOF` is here
        // because it is a bit of `flags` - the `0x8000` in `EV_EOF` and the
        // `0x80000000` in `NOTE_EXIT` are the same value's two halves in two
        // different fields, and the guest's `fflags == NOTE_EXIT` test is on the
        // one that is in `fflags`.
        //
        // The **whole** `flags` word is asserted, because it is now known rather
        // than partly guessed: `struct knote` in `bsd/sys/event_private.h` has
        // `#define kn_flags kn_kevent.kei_flags`, so the knote's `flags` *is* the
        // field `kevent_register` filled from the caller's change with
        // `kn->kn_kevent = *(struct kevent_internal_s *)kev;` - which is how the
        // caller's own `EV_ADD` gets into the delivered word - and the filter's
        // three additions land in that same field. See [`PROC_DELIVERED_FLAGS`]
        // for the chain. `EV_ERROR` is asserted absent too, because that one
        // would mean the registration itself had failed.
        assert_eq!(
            got[0].flags,
            EV_ADD | EV_ONESHOT | EV_CLEAR | EV_EOF,
            "the delivered flags is the caller's own EV_ADD with the kernel's three additions"
        );
        assert_eq!(got[0].flags & EV_ERROR, 0, "and this is not an error event");
        // `data` is zero and that is XNU's `kn_hook32` for a registration that
        // did not ask for NOTE_EXITSTATUS or NOTE_EXIT_DETAIL, both of which
        // are refused. A non-zero here would be a fabricated exit status.
        assert_eq!(got[0].data, 0);

        // One-shot: `filt_procevent` sets EV_ONESHOT and the knote was attached
        // to a `struct proc` that is gone, so there is nothing left to wait on.
        // The second wait is a collection that finds nothing, and it is not a
        // second event - which is what would happen if this were left armed,
        // because a `pidfd` stays readable forever.
        assert!(collect(queue, 1).is_empty());
        assert_eq!(
            apply_one(queue, &watch_proc(pid, EV_DELETE)),
            Err(libc::ENOENT),
            "and the registration is gone rather than re-armable"
        );

        // SAFETY: this test's own queue descriptor, and a child of ours.
        unsafe {
            libc::close(queue);
            let mut status: c_int = 0;
            libc::waitpid(pid, &raw mut status, 0);
        }
    }

    /// `EV_DELETE` removes the registration and closes the `pidfd`, and
    /// cancelling while another thread waits does not close it underneath.
    #[test]
    fn deleting_a_proc_registration_closes_its_pidfd() {
        let queue = queue();
        let pid = child_that_exits(/* reap */ false);
        assert_eq!(apply_one(queue, &watch_proc(pid, EV_ADD)), Ok(()));

        // The descriptor the registration is holding, so that "it was closed"
        // is a measurement rather than an inference.
        let (fd, strong) = {
            let state = queues()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&queue)
                .cloned()
                .expect("the queue is in this module's table");
            let state = state.lock().unwrap_or_else(|e| e.into_inner());
            let proc = state.procs.get(&(pid as u32 as u64)).expect("registered");
            (proc.fd.0, Arc::strong_count(&proc.fd))
        };
        assert!(strong >= 1);

        assert_eq!(apply_one(queue, &watch_proc(pid, EV_DELETE)), Ok(()));
        // Dropping the queue's `Arc<PidFd>` was the last reference, so the
        // descriptor is closed: `F_GETFD` is `EBADF` on a number that is not
        // open, and the number has not been reused because nothing else in this
        // process opened a descriptor since.
        // SAFETY: `F_GETFD` reads no argument and writes no memory.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        assert_eq!(crate::errno::get(), libc::EBADF);

        // And the second delete is ENOENT, which is the check that catches a
        // delete of a registration that was never made.
        assert_eq!(apply_one(queue, &watch_proc(pid, EV_DELETE)), Err(libc::ENOENT));

        // SAFETY: this test's own queue descriptor, and a child of ours.
        unsafe {
            libc::close(queue);
            let mut status: c_int = 0;
            libc::waitpid(pid, &raw mut status, 0);
        }
    }

    /// A pid that is not there is `ESRCH`, which is the reference's own answer
    /// and the one both of the framework's callers act on.
    #[test]
    fn a_pid_that_does_not_exist_is_esrch_which_the_callers_read_as_dead() {
        let queue = queue();
        // A child that has exited *and* been reaped: the pid is free, so
        // `pidfd_open` cannot open it. `pidfd_open` on a *zombie* succeeds -
        // the zombie is still a process - which is why this one reaps.
        let pid = child_that_exits(/* reap */ true);
        assert_eq!(
            apply_one(queue, &watch_proc(pid, EV_ADD)),
            Err(libc::ESRCH),
            "filt_procattach answers ESRCH for an ident proc_find cannot find, and \
             base::WaitForSingleNonChildProcess returns true on ESRCH"
        );

        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// A `pidfd` stays readable forever, so an event that did not fit in
    /// `nevents` must not be lost. This is the trap: a `timerfd` can be read
    /// to clear it, and a `pidfd` has nothing to read.
    #[test]
    fn a_proc_event_that_did_not_fit_is_reported_by_the_next_wait() {
        let queue = queue();
        // Two children, so two registrations, and the first to exit is whichever
        // the kernel says - the ordering is by `ident`, so this does not depend
        // on which.
        let first = child_that_exits(/* reap */ false);
        let second = child_that_exits(/* reap */ false);
        assert_eq!(apply_one(queue, &watch_proc(first, EV_ADD)), Ok(()));
        assert_eq!(apply_one(queue, &watch_proc(second, EV_ADD)), Ok(()));

        // One slot for two events: one is delivered and the other is not.
        let got = collect(queue, 1);
        assert_eq!(got.len(), 1);
        let delivered = got[0].ident;

        // The other is still there, and the next wait says so. If the
        // registration had been dropped on delivery this would be empty, and the
        // fact that a process exited would be gone with it.
        let got = collect(queue, 1);
        assert_eq!(got.len(), 1, "the event that did not fit is still pending");
        assert_ne!(got[0].ident, delivered);

        // SAFETY: this test's own queue descriptor, and two children of ours.
        unsafe {
            libc::close(queue);
            for pid in [first, second] {
                let mut status: c_int = 0;
                libc::waitpid(pid, &raw mut status, 0);
            }
        }
    }

    /// A change that only re-states a registration is `ENOENT` when there is
    /// none, and never a watch.
    ///
    /// `EV_RECEIPT` alone is the form that makes the question possible at all: a
    /// caller applying a batch asks for the outcome of every change, and the
    /// outcome of this one has to be "there is no such registration" rather than
    /// "watching". `kevent_register` says so with the `kn == NULL &&
    /// (kev->flags & EV_ADD) == 0` arm, and a success receipt here would be a
    /// watch the caller never asked for.
    #[test]
    fn a_receipt_for_a_proc_registration_that_was_never_made_is_enoent() {
        let queue = queue();
        let receipt = Kevent64 {
            flags: EV_RECEIPT,
            ..watch_proc(self_pid(), EV_ADD)
        };
        let mut got = [Kevent64::default(); 1];
        // SAFETY: one change, room for the one receipt, and a zero timeout.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::from_ref(&receipt),
                1,
                got.as_mut_ptr(),
                1,
                0,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, 1, "a receipt answers with an event, not a return value");
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
        assert_eq!(got[0].data as i32, crate::errno::from_host(libc::ENOENT));
        // And nothing was registered, so there is no `pidfd` to close.
        let (n, _) = call(queue, &[], 1);
        assert_eq!(n, 0, "and no registration was made out of it");
        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// The `fflags` this filter will not report, and the two it must refuse
    /// with the *reference's* numbers rather than its own.
    ///
    /// **`#[test]` was on the end of this doc comment rather than on its own
    /// line**, so the attribute was part of the documentation, this function was
    /// not a test, and the compiler said so - `warning: function
    /// proc_notes_a_pidfd_cannot_report_are_refused_by_name is never used`. A
    /// fully written test that never runs is the same shape of lie as one that
    /// asserts nothing, and the brief records this tree lying three ways already.
    /// It runs now, and it passes.
    #[test]
    fn proc_notes_a_pidfd_cannot_report_are_refused_by_name() {
        let queue = queue();

        // `NOTE_EXITSTATUS` is the one `<sys/event.h>` documents as putting the
        // wait(2) status word in `data`. The only way to learn it is to reap,
        // and reaping is what the framework's own `waitpid` calls collect - so
        // this is refused rather than answered with a zero that would read as
        // "exited with status 0".
        let mut status = watch_proc(self_pid(), EV_ADD);
        status.fflags = NOTE_EXIT | NOTE_EXITSTATUS;
        assert_eq!(apply_one(queue, &status), Err(libc::EINVAL));

        // The jetsam / CSKILLED / decrypt-fail reasons: fields of a macOS
        // `struct proc` that a Linux one does not have.
        let mut detail = watch_proc(self_pid(), EV_ADD);
        detail.fflags = NOTE_EXIT | NOTE_EXIT_DETAIL;
        assert_eq!(apply_one(queue, &detail), Err(libc::EINVAL));

        // The process lifecycle. `NOTE_FORK` is 0x40000000 and `NOTE_EXEC` is
        // 0x20000000; neither is something a descriptor reports.
        for note in [0x4000_0000u32, 0x2000_0000, 0x1000_0000, 0x0000_0010] {
            let mut lifecycle = watch_proc(self_pid(), EV_ADD);
            lifecycle.fflags = NOTE_EXIT | note;
            assert_eq!(
                apply_one(queue, &lifecycle),
                Err(libc::EINVAL),
                "fflags {note:#x} should be refused"
            );
        }

        // The three "follow it across fork" notes are refused by the reference
        // itself, with `ENOTSUP`, *before* it looks at the pid - and before the
        // pid here is a valid one, which is the point: the order is XNU's.
        for note in [NOTE_TRACK, NOTE_TRACKERR, NOTE_CHILD] {
            let mut track = watch_proc(self_pid(), EV_ADD);
            track.fflags = note;
            assert_eq!(
                apply_one(queue, &track),
                Err(libc::ENOTSUP),
                "filt_procattach answers ENOTSUP for {note:#x}, and the guest sees that number"
            );
        }

        // `EV_EOF` and `EV_ERROR` are what the kernel *returns*; passing them
        // in a change is refused, as it is for every other filter here.
        assert_eq!(
            apply_one(queue, &watch_proc(self_pid(), EV_ADD | EV_EOF)),
            Err(libc::EINVAL)
        );

        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// `EV_DISABLE` takes a registration out of the wait and `EV_ENABLE` puts
    /// it back, and neither disturbs the `pidfd` - the process has not stopped
    /// existing while a knote was disabled, and the descriptor is what the knote
    /// *is*.
    #[test]
    fn a_disabled_proc_registration_is_not_polled_and_comes_back_intact() {
        let queue = queue();
        let pid = child_that_exits(/* reap */ false);
        assert_eq!(apply_one(queue, &watch_proc(pid, EV_ADD | EV_DISABLE)), Ok(()));

        // The child has exited, but the registration is disabled, so nothing is
        // reported. This is a real wait, not a collection: a `poll` on the
        // disabled registration's descriptor would have been the bug.
        static A_FIFTH: libc::timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 200_000_000,
        };
        let mut got = vec![Kevent64::default(); 1];
        // SAFETY: no changes, one event slot, a 200 ms timeout.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                0,
                &raw const A_FIFTH,
            )
        };
        assert_eq!(n, 0, "a disabled registration is not reported");

        // `EV_ENABLE` with no `EV_ADD`, which is the form that changes an
        // existing registration rather than creating one.
        let mut enable = watch_proc(pid, EV_ENABLE);
        enable.fflags = 0;
        assert_eq!(apply_one(queue, &enable), Ok(()));

        let got = collect(queue, 1);
        assert_eq!(got.len(), 1, "and now it is reported");
        assert_eq!(got[0].ident, pid as u32 as u64);

        // SAFETY: this test's own queue descriptor, and a child of ours.
        unsafe {
            libc::close(queue);
            let mut status: c_int = 0;
            libc::waitpid(pid, &raw mut status, 0);
        }
    }

    /// The exact shape of `WaitManyImpl`: one queue, `EVFILT_MACHPORT` for
    /// every waitable, the index in `udata`, no timeout.
    #[test]
    fn a_port_with_a_message_reports_the_index_its_owner_gave_it() {
        let queue = queue();
        let first = receive_right();
        let second = receive_right();

        // Nothing signalled yet: a collection, not a wait, and the answer is
        // zero. Chromium's `PCHECK(rv > 0)` is not on this path because it
        // never asks.
        let (n, _) = call(queue, &[add(first, 0), add(second, 1)], 2);
        assert_eq!(n, 0, "neither port has a message");

        signal(second);
        let (n, got) = call(queue, &[], 2);
        assert_eq!(n, 1, "one port is ready");
        assert_eq!(got[0].ident, second as u64);
        assert_eq!(got[0].filter, EVFILT_MACHPORT);
        assert_eq!(
            got[0].udata, 1,
            "the caller's index, not the port's position"
        );
        assert_eq!(
            got[0].data, second as i64,
            "data is the port with the message"
        );
        assert_eq!(got[0].flags & EV_ERROR, 0);

        // ... and the same call again, because nothing was taken.
        let (n, _) = call(queue, &[], 2);
        assert_eq!(
            n, 1,
            "the message is still queued: this is a poll, not a receive"
        );

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(first);
            space::destroy(second);
        }
    }

    /// The lowest index is what `WaitManyImpl` returns, so two ready ports have
    /// to arrive in one call and both have to be there.
    #[test]
    fn two_ready_ports_are_one_call_and_both_are_reported() {
        let queue = queue();
        let first = receive_right();
        let second = receive_right();
        signal(first);
        signal(second);
        let (n, got) = call(queue, &[add(first, 0), add(second, 1)], 2);
        assert_eq!(n, 2);
        let udatas: Vec<u64> = got.iter().map(|event| event.udata).collect();
        assert_eq!(udatas, vec![0, 1]);

        // A room too small to hold both reports what fits, and drops the rest,
        // which is what Darwin does: `nevents` is a capacity, not a request.
        let (n, got) = call(queue, &[], 1);
        assert_eq!(n, 1);
        assert_eq!(got[0].udata, 0);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(first);
            space::destroy(second);
        }
    }

    /// A port set is a filter too, and `data` is the member rather than the set.
    #[test]
    fn a_port_set_reports_the_member_that_has_the_message() {
        let queue = queue();
        let set = port_set();
        let member = receive_right();
        assert_eq!(
            space::mach_port_insert_member(space::TASK_SELF, member, set),
            0
        );

        let (n, _) = call(queue, &[add(set, 0)], 1);
        assert_eq!(n, 0);
        signal(member);
        let (n, got) = call(queue, &[], 1);
        assert_eq!(n, 1);
        assert_eq!(got[0].ident, set as u64, "the event is the registration's");
        assert_eq!(got[0].data, member as i64, "and data names the member");

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(set);
            space::destroy(member);
        }
    }

    /// The contract `mach_port_peek` and `EVFILT_MACHPORT` share: **one event
    /// when there is a message, none when there is not.** A `kevent64_t` that
    /// claims a port is readable when its queue is empty is what sends Chromium
    /// into a `mach_msg` that never returns.
    #[test]
    fn a_reported_event_always_has_a_message_behind_it() {
        let queue = queue();
        let port = receive_right();
        call(queue, &[add(port, 0)], 1);

        for round in 0..3 {
            signal(port);
            let (n, _) = call(queue, &[], 1);
            assert_eq!(n, 1, "round {round}");

            // The same question `mach_port_peek` is asked, and it has to agree.
            let (mut seqno, mut size, mut id, mut trailer) = (0u32, 0u32, 0u32, [0u8; 8]);
            let mut offered = 8u32;
            // SAFETY: four live out-parameters and an eight-byte trailer buffer.
            let kr = unsafe {
                super::super::msg::mach_port_peek(
                    space::TASK_SELF,
                    port,
                    0,
                    &mut seqno,
                    &mut size,
                    &mut id,
                    trailer.as_mut_ptr(),
                    &mut offered,
                )
            };
            assert_eq!(kr, 0, "round {round}: kevent64 said ready and peek agrees");

            // Take it, and the event goes away.
            let mut buf = vec![0u8; 64];
            // SAFETY: 64 writable bytes.
            let rc = unsafe {
                super::super::msg::mach_msg(
                    buf.as_mut_ptr().cast(),
                    0x2 | 0x100, // MACH_RCV_MSG | MACH_RCV_TIMEOUT
                    0,
                    buf.len() as u32,
                    port,
                    1000,
                    space::MACH_PORT_NULL,
                )
            };
            assert_eq!(rc, 0, "round {round}: the message was received");
            let (n, _) = call(queue, &[], 1);
            assert_eq!(n, 0, "round {round}: and no longer reported");
        }

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// `EV_ONESHOT` is how a caller asks not to be handed the same readiness on
    /// every turn, and it has to be honoured or the caller loops.
    #[test]
    fn a_one_shot_registration_fires_once() {
        let queue = queue();
        let port = receive_right();
        let mut change = add(port, 0);
        change.flags = EV_ADD | EV_ONESHOT;
        assert_eq!(call(queue, &[change], 1).0, 0);

        signal(port);
        assert_eq!(call(queue, &[], 1).0, 1, "it fired");
        // The message is still there and the registration is not, so this is
        // zero - and `EV_DELETE` of it is now `ENOENT`.
        assert_eq!(call(queue, &[], 1).0, 0);
        let mut delete = add(port, 0);
        delete.flags = EV_DELETE;
        assert_eq!(call(queue, &[delete], 1).0, -1, "ENOENT");

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// `EV_CLEAR` on a port is accepted and changes nothing, because a port's
    /// event is level-triggered by construction: the message that fired it is
    /// still at the head of the queue. Only a filter that *consumes* its event
    /// - a timer, a user event - has anything to clear.
    #[test]
    fn a_cleared_port_registration_still_fires_every_turn() {
        let queue = queue();
        let port = receive_right();
        let mut change = add(port, 0);
        change.flags = EV_ADD | EV_CLEAR;
        assert_eq!(call(queue, &[change], 1).0, 0);

        signal(port);
        assert_eq!(call(queue, &[], 1).0, 1);
        assert_eq!(
            call(queue, &[], 1).0,
            1,
            "still there, because it is still there"
        );
        assert_eq!(call(queue, &[], 1).0, 1);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// `EV_DISABLE` takes a registration out of the set without deleting it, and
    /// `EV_ENABLE` puts it back.
    #[test]
    fn a_disabled_registration_is_not_reported_until_it_is_enabled() {
        let queue = queue();
        let port = receive_right();
        let mut disable = add(port, 0);
        disable.flags = EV_DISABLE;
        call(queue, &[add(port, 0), disable], 2);

        signal(port);
        assert_eq!(call(queue, &[], 1).0, 0, "a disabled knote does not fire");
        let mut enable = add(port, 0);
        enable.flags = EV_ENABLE;
        call(queue, &[enable], 1);
        assert_eq!(call(queue, &[], 1).0, 1, "and the message was still queued");

        // Enabling something that is not there is `ENOENT`, which is the check
        // that catches a filter applied to a queue that never heard of it.
        let mut orphan = add(port, 1);
        orphan.ident = 0x7fff_0000;
        orphan.flags = EV_ENABLE;
        assert_eq!(call(queue, &[orphan], 1).0, -1);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// A port that is destroyed while a queue still names it must not be waited
    /// on forever, and must not be reported as readable.
    #[test]
    fn a_registration_whose_port_is_destroyed_is_reported_once_and_dropped() {
        let queue = queue();
        let port = receive_right();
        assert_eq!(call(queue, &[add(port, 0)], 1).0, 0);
        space::destroy(port);

        let (n, got) = call(queue, &[], 1);
        assert_eq!(n, 1, "the wait was woken rather than left to hang");
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
        assert_eq!(got[0].data as i32, crate::errno::from_host(libc::EBADF));
        assert_eq!(got[0].udata, 0, "so the caller can tell which one it was");
        // Dropped, not reported again.
        assert_eq!(
            call(queue, &[], 1).0,
            0,
            "and then there is nothing left to report"
        );

        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// A filter this file will not answer is refused by number, and
    /// `EV_RECEIPT` is how the caller finds out which change it was.
    #[test]
    fn a_filter_that_is_not_a_mach_port_is_refused_by_number() {
        let queue = queue();
        let mut change = Kevent64 {
            ident: 1,
            filter: EVFILT_VNODE,
            flags: EV_ADD | EV_RECEIPT,
            ..Kevent64::default()
        };
        let (n, got) = call(queue, &[change], 1);
        assert_eq!(n, 1);
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
        assert_eq!(got[0].data as i32, crate::errno::from_host(libc::EINVAL));

        // Without `EV_RECEIPT` the whole call fails.
        change.flags = EV_ADD;
        assert_eq!(call(queue, &[change], 1).0, -1);
        assert_eq!(
            crate::errno::get(),
            crate::errno::to_host(crate::errno::from_host(libc::EINVAL))
        );

        // And the filter this file does answer is still answerable afterwards.
        let port = receive_right();
        assert_eq!(call(queue, &[add(port, 0)], 1).0, 0);
        signal(port);
        assert_eq!(call(queue, &[], 1).0, 1);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// An `ident` that is not a port has no queue to watch, and registering it
    /// anyway would be a knote that can never fire.
    #[test]
    fn a_registration_of_something_that_is_not_a_port_is_refused() {
        let queue = queue();
        for ident in [
            space::TASK_SELF, // a special name
            0x7fff_0000u32,   // a name this namespace never issued
            u32::MAX,         // MACH_PORT_DEAD
        ] {
            let mut change = add(ident, 0);
            change.flags = EV_ADD | EV_RECEIPT;
            let (n, got) = call(queue, &[change], 1);
            assert_eq!(n, 1, "ident {ident:#x} reports its failure");
            assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
            assert_eq!(got[0].data as i32, crate::errno::from_host(libc::EINVAL));
        }
        // A send right is a name, and not a receive right.
        let mut receive = space::MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { space::mach_port_allocate(space::TASK_SELF, space::RIGHT_RECEIVE, &mut receive) };
        let (mut send, mut kind) = (space::MACH_PORT_NULL, 0);
        // SAFETY: two live locals for the out-parameters.
        unsafe {
            space::mach_port_extract_right(
                space::TASK_SELF,
                receive,
                super::super::msg::MAKE_SEND,
                &mut send,
                &mut kind,
            )
        };
        let mut change = add(send, 0);
        change.flags = EV_ADD | EV_RECEIPT;
        let (n, got) = call(queue, &[change], 1);
        assert_eq!(n, 1);
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::mach_port_deallocate(space::TASK_SELF, send);
            space::destroy(receive);
        }
    }

    /// A descriptor that is not a queue is `EBADF`, and that answer comes from
    /// `kqueue`'s own table rather than from guessing at the descriptor.
    #[test]
    fn a_descriptor_that_is_not_a_queue_is_refused() {
        let mut got = vec![Kevent64::default(); 1];
        // SAFETY: standard input is open and is not a kqueue.
        let n = unsafe {
            kevent64(
                0,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                0,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, -1);
        assert_eq!(crate::errno::get(), libc::EBADF);
    }

    /// The seventh argument is a `timespec *` and the sixth is a value.
    ///
    /// `KEVENT_FLAG_IMMEDIATE` is the one bit of the sixth this answers, and
    /// the test is the guest's own shape: `DoInternalWork` passes a **null**
    /// timeout with the flag set, which without the flag would mean "block
    /// forever", and `MessagePumpKqueue`'s first pass through `Run` is exactly
    /// that - it wants to be told what is already pending. The flag therefore
    /// has to be an immediate timeout and not an ignored word, or the pump
    /// blocks where the guest asked not to.
    #[test]
    fn the_immediate_flag_is_a_zero_timeout_even_with_no_timeout_argument() {
        let queue = queue();
        let port = receive_right();
        call(queue, &[add(port, 0)], 1);

        // A null timeout, no flag: a wait, and nothing to report.
        let mut got = vec![Kevent64::default(); 1];
        // SAFETY: no changes and one event slot, with a null timeout: this call
        // would block forever if the flag below were not what makes it not.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                KEVENT_FLAG_IMMEDIATE,
                std::ptr::null(),
            )
        };
        assert_eq!(n, 0, "nothing was signalled, so the collection is empty");

        // With something pending, the same call reports it without blocking.
        signal(port);
        let mut got = vec![Kevent64::default(); 1];
        // SAFETY: as above.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                KEVENT_FLAG_IMMEDIATE,
                std::ptr::null(),
            )
        };
        assert_eq!(n, 1, "the pending message is reported, and at once");
        assert_eq!(got[0].ident, port as u64);

        // A flag word this does not act on is refused rather than ignored, and
        // the flag that *is* served does not make the rest of the word legal.
        // SAFETY: no changes and one event slot.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                KEVENT_FLAG_IMMEDIATE | 0x4,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// An immediate call on a queue with nothing registered cannot block, so it
    /// collects and reports nothing. Refusing it would be refusing a collection
    /// - and `DoInternalWork` treats `rv == 0` as "no events to dispatch", which
    /// is a normal turn of its loop, not an error.
    #[test]
    fn an_immediate_call_on_an_empty_queue_collects_rather_than_refusing() {
        let queue = queue();
        let mut got = vec![Kevent64::default(); 1];
        // SAFETY: no changes, one event slot, and an immediate flag.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                KEVENT_FLAG_IMMEDIATE,
                std::ptr::null(),
            )
        };
        assert_eq!(n, 0);
        // The same call *without* the flag cannot return at all, so it is
        // refused and named - which is the difference the flag makes.
        // SAFETY: no changes, one event slot and a null timeout.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                0,
                std::ptr::null(),
            )
        };
        assert_eq!(n, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// A `timespec` with an out-of-range `tv_nsec` is `EINVAL` rather than a
    /// wait of a length nobody asked for.
    #[test]
    fn an_impossible_timeout_is_refused() {
        let queue = queue();
        let mut got = vec![Kevent64::default(); 1];
        let bad = libc::timespec {
            tv_sec: 0,
            tv_nsec: 2_000_000_000,
        };
        // SAFETY: no changes and one event slot.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                0,
                &raw const bad,
            )
        };
        assert_eq!(n, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// A wait that cannot ever end is refused, and named, while the same wait
    /// with a timeout in it is Darwin's own answer: no events.
    #[test]
    fn an_untimed_wait_with_nothing_registered_is_refused_rather_than_hung() {
        let queue = queue();
        let mut got = vec![Kevent64::default(); 1];
        // SAFETY: no changes, one event slot and a null timeout.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                0,
                std::ptr::null(),
            )
        };
        assert_eq!(n, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);

        // With a timeout it is a collection that found nothing, which is what
        // Darwin reports and what a caller polling for a change can act on.
        // SAFETY: no changes, one event slot and a zero timeout.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                0,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, 0);
        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// The word the framework stores, read back the way the structure declares
    /// it. If the packing of `kevent64_s` is wrong every one of the tests above
    /// is testing a different function.
    #[test]
    fn the_event_is_48_bytes_with_its_fields_where_the_header_says() {
        assert_eq!(size_of::<Kevent64>(), 48);
        // `mov QWORD PTR [rsi+0x8],0x1fff8` - one store, three fields.
        let mut bytes = vec![0u8; 48];
        bytes[8..16].copy_from_slice(&0x0001_fff8u64.to_le_bytes());
        let mut event = Kevent64::default();
        // SAFETY: 48 bytes, which is the size of the value being read into it.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                (&raw mut event).cast::<u8>(),
                size_of::<Kevent64>(),
            )
        };
        assert_eq!(event.filter, EVFILT_MACHPORT);
        assert_eq!(event.flags, EV_ADD);
        assert_eq!(event.fflags, 0);
    }
}
