mod common;

use common::{patched_call_fixture, run_guest};

#[test]
fn a_patched_call_in_generated_code_reaches_its_new_target() {
    let Some(path) = patched_call_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(out, "before 3 beside 4 after 6 again 3\n", "stderr: {err}");
}

/// A JIT that keeps code writable in one view and executable in another, as
/// one built for W^X does: a named memory entry mapped twice, plus an arena
/// `mach_vm_map` makes executable directly. Self-contained, so it builds
/// without an SDK.
const MACH_VM_MAP_C: &str = r#"
typedef unsigned int mach_port_t;
typedef int kern_return_t;
typedef unsigned long long mach_vm_address_t;

mach_port_t mach_task_self(void);
kern_return_t mach_make_memory_entry_64(mach_port_t, mach_vm_address_t *, mach_vm_address_t,
                                        int, mach_port_t *, mach_port_t);
kern_return_t mach_vm_map(mach_port_t, mach_vm_address_t *, mach_vm_address_t, mach_vm_address_t,
                          int, mach_port_t, mach_vm_address_t, int, int, int, unsigned);
int printf(const char *, ...);

#define VM_PROT_READ 1
#define VM_PROT_WRITE 2
#define VM_PROT_EXECUTE 4
#define VM_FLAGS_ANYWHERE 1
#define VM_INHERIT_NONE 2
#define MAP_MEM_NAMED_CREATE 0x020000
#define SIZE 0x4000
#define CHECK(expr) do { kern_return_t kr = (expr); if (kr) { printf("line %d: %d\n", __LINE__, kr); return 1; } } while (0)

typedef int (*fn)(void);

static void emit_ret(volatile unsigned char *at, int value) {
    at[0] = 0xb8; /* mov eax, imm32 */
    at[1] = value; at[2] = value >> 8; at[3] = value >> 16; at[4] = value >> 24;
    at[5] = 0xc3; /* ret */
}

static kern_return_t map(mach_vm_address_t *at, mach_port_t entry, int prot) {
    *at = 0;
    return mach_vm_map(mach_task_self(), at, SIZE, 0, VM_FLAGS_ANYWHERE, entry, 0, 0, prot,
                       VM_PROT_READ | VM_PROT_WRITE | VM_PROT_EXECUTE, VM_INHERIT_NONE);
}

int main(void) {
    mach_vm_address_t size = SIZE, writable, executable, arena;
    mach_port_t entry;
    int all = VM_PROT_READ | VM_PROT_WRITE | VM_PROT_EXECUTE;
    CHECK(mach_make_memory_entry_64(mach_task_self(), &size, 0, all | MAP_MEM_NAMED_CREATE,
                                    &entry, 0));
    CHECK(map(&writable, entry, VM_PROT_READ | VM_PROT_WRITE));
    CHECK(map(&executable, entry, VM_PROT_READ | VM_PROT_EXECUTE));
    emit_ret((unsigned char *)writable, 42);
    emit_ret((unsigned char *)writable + 0x1000, 7);
    CHECK(map(&arena, 0, all));
    emit_ret((unsigned char *)arena, 5);
    printf("aliased %d %d arena %d\n", ((fn)executable)(), ((fn)(executable + 0x1000))(),
           ((fn)arena)());
    return 0;
}
"#;

#[test]
fn code_mapped_by_mach_vm_map_runs_from_its_executable_view() {
    let Some(path) = common::build_x86_64("mach_vm_map", MACH_VM_MAP_C, &["-O1"]) else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(out, "aliased 42 7 arena 5\n", "stderr: {err}");
}
