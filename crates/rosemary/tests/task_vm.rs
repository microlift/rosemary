//! One guest reading, writing and suspending another through Mach task ports,
//! which the process server carries to the target's own service handler.

mod common;

/// Self-contained, so it builds without an SDK as well as with one: a forked
/// child keeps its parent's addresses, which is how the parent knows where to
/// look in it.
pub const SOURCE: &str = r#"
typedef unsigned int mach_port_t;
typedef int kern_return_t;
typedef unsigned long long mach_vm_address_t;
typedef unsigned long vm_offset_t;

mach_port_t mach_task_self(void);
kern_return_t task_for_pid(mach_port_t, int, mach_port_t *);
kern_return_t mach_vm_read(mach_port_t, mach_vm_address_t, mach_vm_address_t, vm_offset_t *, unsigned int *);
kern_return_t mach_vm_write(mach_port_t, mach_vm_address_t, vm_offset_t, unsigned int);
kern_return_t mach_vm_deallocate(mach_port_t, mach_vm_address_t, mach_vm_address_t);
kern_return_t task_suspend(mach_port_t);
kern_return_t task_resume(mach_port_t);
int fork(void);
int getpid(void);
int getppid(void);
int close(int);
int pipe(int[2]);
long read(int, void *, unsigned long);
long write(int, const void *, unsigned long);
int waitpid(int, int *, int);
int usleep(unsigned int);
int kill(int, int);
int execv(const char *, char *const[]);
void _exit(int);
int printf(const char *, ...);
int strcmp(const char *, const char *);
void *signal(int, void (*)(int));

#define KERN_SUCCESS 0
#define KERN_INVALID_ADDRESS 1
#define KERN_PROTECTION_FAILURE 2
#define MACH_SEND_INVALID_DEST 0x10000003
#define SIGKILL 9
#define SIGCONT 19
#define CHECK(expr) do { if (!(expr)) { printf("failed line %d: %s\n", __LINE__, #expr); return __LINE__; } } while (0)

static volatile long counter;
static volatile char marker[16] = "child-side";
static volatile int continued;
static int parent;
static const char constant[16] = "read-only";

static void on_continue(int signo) { (void)signo; continued = 1; }

static long counter_of(mach_port_t task) {
    vm_offset_t data;
    unsigned int count;
    if (mach_vm_read(task, (mach_vm_address_t)&counter, sizeof counter, &data, &count) != KERN_SUCCESS)
        return -1;
    long value = *(long *)data;
    mach_vm_deallocate(mach_task_self(), data, count);
    return value;
}

static int spin(int ready) {
    signal(SIGCONT, on_continue);
    write(ready, "r", 1);
    // A parent that gave up never writes the marker; without it the child
    // spins forever and holds the test's output pipe open.
    while (strcmp((const char *)marker, "parent-wrote") != 0) {
        if (getppid() != parent) _exit(4);
        counter++;
    }
    // Let the parent check it runs again before going.
    for (long i = 0; i < 50000000; i++) counter++;
    _exit(continued ? 3 : 0);
}

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "exec-target")) {
        usleep(5000000);
        return 0;
    }
    parent = getpid();
    int ready[2];
    CHECK(pipe(ready) == 0);
    int child = fork();
    CHECK(child >= 0);
    if (child == 0) spin(ready[1]);
    char byte;
    CHECK(read(ready[0], &byte, 1) == 1);

    mach_port_t task;
    CHECK(task_for_pid(mach_task_self(), child, &task) == KERN_SUCCESS);

    vm_offset_t data;
    unsigned int count;
    CHECK(mach_vm_read(task, (mach_vm_address_t)marker, sizeof marker, &data, &count) == KERN_SUCCESS);
    CHECK(count == sizeof marker && !strcmp((const char *)data, "child-side"));
    CHECK(mach_vm_deallocate(mach_task_self(), data, count) == KERN_SUCCESS);
    CHECK(mach_vm_read(task, 16, 4, &data, &count) == KERN_INVALID_ADDRESS);
    CHECK(mach_vm_write(task, (mach_vm_address_t)constant, (vm_offset_t)"x", 2) == KERN_PROTECTION_FAILURE);

    // Suspended, it stops counting and still answers.
    CHECK(task_suspend(task) == KERN_SUCCESS);
    long before = counter_of(task);
    usleep(100000);
    CHECK(before >= 0 && counter_of(task) == before);
    static const char wrote[16] = "parent-wrote";
    CHECK(mach_vm_write(task, (mach_vm_address_t)marker, (vm_offset_t)wrote, sizeof wrote) == KERN_SUCCESS);
    CHECK(mach_vm_read(task, (mach_vm_address_t)marker, sizeof marker, &data, &count) == KERN_SUCCESS);
    CHECK(!strcmp((const char *)data, "parent-wrote"));
    // A second suspension only counts.
    CHECK(task_suspend(task) == KERN_SUCCESS);
    CHECK(task_resume(task) == KERN_SUCCESS);
    usleep(100000);
    CHECK(counter_of(task) == before);
    CHECK(task_resume(task) == KERN_SUCCESS);
    CHECK(task_resume(task) != KERN_SUCCESS);
    int status;
    CHECK(waitpid(child, &status, 0) == child);
    CHECK(status == 0);

    // A task port names one image: after an exec it is dead.
    int go[2];
    CHECK(pipe(go) == 0);
    child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        // Only the parent's end left, so a parent that gave up is EOF.
        close(go[1]);
        write(ready[1], "r", 1);
        read(go[0], &byte, 1);
        char *args[] = {argv[0], "exec-target", 0};
        execv(argv[0], args);
        _exit(1);
    }
    CHECK(read(ready[0], &byte, 1) == 1);
    CHECK(task_for_pid(mach_task_self(), child, &task) == KERN_SUCCESS);
    CHECK(counter_of(task) >= 0);
    CHECK(write(go[1], "g", 1) == 1);
    kern_return_t rc = KERN_SUCCESS;
    for (int i = 0; i < 500 && rc != MACH_SEND_INVALID_DEST; i++) {
        rc = mach_vm_read(task, (mach_vm_address_t)marker, sizeof marker, &data, &count);
        usleep(10000);
    }
    CHECK(rc == MACH_SEND_INVALID_DEST);
    kill(child, SIGKILL);
    waitpid(child, &status, 0);
    printf("task-vm=ok\n");
    return 0;
}
"#;

#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "a macOS host has no realtime signal for the task service handler; see darwin::mach::service::install"
)]
fn a_guest_reads_writes_and_suspends_another() {
    let built = [
        (
            "emulated",
            common::build_x86_64("task_vm", SOURCE, &[]),
            "--emulated",
        ),
        (
            "native",
            common::build_native("task_vm_native", SOURCE),
            "--native",
        ),
    ];
    if built.iter().all(|(_, image, _)| image.is_none()) {
        skip!("cannot build Mach-O fixtures");
    }
    for (name, image, engine) in built {
        let Some(image) = image else {
            continue;
        };
        let output = common::driver_command()
            .env("ROSEMARY_PREFIX", "off")
            .env("ROSEMARY_SERVER", "on")
            .env("ROSEMARY_SERVER_LINGER", "1")
            .args(["run", engine, &image])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("task-vm=ok"),
            "{name}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
