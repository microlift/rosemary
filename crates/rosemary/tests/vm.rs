mod common;

use rosemary::vm::GuestView;
use rosemary::{Program, Vm, startup};

use rosemary::abi::GuestMemory;

fn vm_for(path: &str) -> &'static Vm {
    let program = Program::load_executable(path, &[])
        .unwrap_or_else(|e| panic!("linking {path} failed: {e}"));
    // Two arguments, so `argc` and `argv` agree: a guest handed an `argc` its
    // `argv` cannot back reads past the array's NULL and dereferences it - its
    // own behaviour, correctly reproduced, and not a failure worth
    // manufacturing in a test.
    let args = [
        std::ffi::OsString::from(path),
        std::ffi::OsString::from("one"),
    ];
    Box::leak(Box::new(
        Vm::new(program, path, &args).expect("a VM with a stack"),
    ))
}

#[test]
fn a_region_is_translated_once_and_reused() {
    // The cache's whole claim. A guest with a loop must not translate the same
    // region on every iteration, and running the same program twice from the
    // same VM must translate nothing new the second time.
    let Some(path) = common::qsort_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let vm = vm_for(&path);

    startup::run_initializers(vm).expect("initializers run");
    let entry = vm
        .program
        .entry()
        .expect("an executable has an entry point");
    let (argc, argv, envp, apple) = vm.main_args();

    vm.call(entry, &[argc, argv, envp, apple])
        .expect("main runs");
    let after_first = vm.translated();
    // A region ends at every `ret`, indirect jump and call, so any program with
    // more than one function comes to more than one region. See
    // `rosemary::region`.
    assert!(
        after_first > 1,
        "a whole program in one region is not a JIT"
    );

    vm.call(entry, &[argc, argv, envp, apple])
        .expect("main runs again");
    assert_eq!(
        vm.translated(),
        after_first,
        "the second run translated something new"
    );
}

#[test]
fn one_guest_pointer_gets_exactly_one_thunk() {
    // Host code compares function pointers - `atexit` deduplicates, a dispatch
    // table is checked against a sentinel - so two thunks for one guest
    // function would answer a question the guest can ask, wrongly.
    let Some(path) = common::qsort_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let vm = vm_for(&path);
    let entry = vm.program.entry().expect("an entry point");

    let first = vm.thunk_for(entry).expect("a thunk for the entry point");
    let second = vm.thunk_for(entry).expect("the same thunk again");
    assert_eq!(first, second);
    assert_eq!(vm.thunks(), 1);

    // And it is reversible, which is what stops the guest calling its own
    // function pointer through a trampoline back into itself.
    assert_eq!(vm.guest_behind_thunk(first), Some(entry));
    assert_eq!(
        vm.guest_behind_thunk(entry),
        None,
        "guest code is not a thunk"
    );
}

#[test]
fn a_thunk_is_a_native_function_the_host_can_call() {
    // Called the way a host library would: through a C function pointer, with
    // no runtime involvement at the call site. `main(argc, argv, envp, apple)`
    // returns `abs(argc) - 1`, so this also checks the argument placement and
    // the return value rather than only that it did not crash.
    let Some(path) = common::hello() else {
        skip!("cannot build x86_64 fixtures")
    };
    let vm = vm_for(&path);
    let entry = vm.program.entry().expect("an entry point");
    let (argc, argv, envp, apple) = vm.main_args();

    let thunk = vm.thunk_for(entry).expect("a thunk");
    // Published, because a thunk entered from outside the runtime has to find
    // the VM by itself. See `rosemary::thunk::publish`.
    rosemary::thunk::publish(vm);

    // SAFETY: `thunk` is the address of a trampoline this VM compiled, whose
    // prototype is six integer arguments and an integer return - see
    // `rosemary::thunk`. Passing fewer than six is what AAPCS64 permits and
    // what every C caller does.
    let f: extern "C" fn(u64, u64, u64, u64) -> u64 = unsafe { std::mem::transmute(thunk) };
    assert_eq!(
        f(argc, argv, envp, apple),
        1,
        "abs(argc) - 1, with two arguments"
    );
}

#[test]
fn an_upgraded_region_runs_as_the_provisional_one_did() {
    let Some(path) = common::hello() else {
        skip!("cannot build x86_64 fixtures")
    };
    if !rosemary::jit::tiered() {
        skip!("tiered compilation is off in this environment")
    }
    let vm = vm_for(&path);
    vm.start_tiering();
    let entry = vm.program.entry().expect("an entry point");
    let (argc, argv, envp, apple) = vm.main_args();

    assert_eq!(
        vm.call(entry, &[argc, argv, envp, apple])
            .expect("main runs"),
        1
    );
    assert!(vm.is_provisional(entry), "a first lift is provisional");
    let regions = vm.translated();

    vm.upgrade(entry);
    assert!(!vm.is_provisional(entry), "the upgrade replaced it");
    assert_eq!(
        vm.translated(),
        regions,
        "a replacement, not a second region"
    );
    assert_eq!(
        vm.call(entry, &[argc, argv, envp, apple])
            .expect("main runs again"),
        1
    );
}

#[test]
fn the_guest_stack_is_readable_guest_memory() {
    // Shims bounds-check the pointers they are handed, and a `char buf[64]` is
    // on the stack - so a stack that is not part of the guest's memory would
    // make every `snprintf` into a buffer fail.
    let Some(path) = common::hello() else {
        skip!("cannot build x86_64 fixtures")
    };
    let vm = vm_for(&path);
    let (_, argv, _, _) = vm.main_args();

    let view: GuestView = vm.view().clone();
    assert!(view.contains(argv), "argv is on the guest stack");

    let mut word = [0u8; 8];
    view.read(argv, &mut word).expect("argv[0] is readable");
    let argv0 = u64::from_le_bytes(word);
    let mem: &dyn GuestMemory = &view;
    let text = mem
        .read_cstr(argv0, "argv[0]")
        .expect("argv[0] names the program");
    assert_eq!(text, path.as_bytes());
}

#[test]
fn an_address_in_nothing_mapped_is_refused_rather_than_dereferenced() {
    let Some(path) = common::hello() else {
        skip!("cannot build x86_64 fixtures")
    };
    let vm = vm_for(&path);
    let view = vm.view().clone();

    let mut byte = [0u8; 1];
    assert!(view.read(0, &mut byte).is_err(), "null is not readable");
    assert!(
        view.read(0x7fff_0000_0000_0000, &mut byte).is_err(),
        "nor is a wild address"
    );
}

#[test]
fn entering_an_address_that_is_not_code_fails_by_address() {
    let Some(path) = common::hello() else {
        skip!("cannot build x86_64 fixtures")
    };
    let vm = vm_for(&path);

    let err = vm
        .call(0xdead_beef, &[])
        .expect_err("that is not guest code");
    assert!(
        err.to_string().contains("deadbeef"),
        "the stop should name the address: {err}"
    );
}

#[test]
fn a_guest_that_returns_a_value_returns_it() {
    // The narrowest possible use of the loop: enter a known guest function,
    // come back with `rax`. Everything else here is built on this working.
    let Some(path) = common::hello() else {
        skip!("cannot build x86_64 fixtures")
    };
    let vm = vm_for(&path);
    let entry = vm.program.entry().expect("an entry point");
    let (argc, argv, envp, apple) = vm.main_args();

    assert_eq!(
        vm.call(entry, &[argc, argv, envp, apple])
            .expect("main runs"),
        1
    );
}
