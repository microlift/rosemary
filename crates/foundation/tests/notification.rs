use std::sync::atomic::{AtomicUsize, Ordering};

use objc::object::Id;
use objc::sel::Sel;

const CHILD: &str = "ROSEMARY_FOUNDATION_NOTIFICATION_CHILD";
const NAME: &str = "RosemaryTestObserverGone";

static HEARD: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn heard(_receiver: Id, _sel: Sel, _notification: Id) {
    HEARD.fetch_add(1, Ordering::SeqCst);
}

fn send0(receiver: Id, selector: &str) -> Id {
    // SAFETY: every selector sent here is `id (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector))
    }
}

fn center() -> Id {
    let class = objc::class::lookup(c"NSNotificationCenter").expect("NSNotificationCenter");
    send0(objc::class::ptr(class) as Id, "defaultCenter")
}

fn add_observer(observer: Id, name: Id) {
    // SAFETY: `-addObserver:selector:name:object:` is `void (id, SEL, id, SEL,
    // id, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Sel, Id, Id) =
            std::mem::transmute(objc::msg::entry());
        f(
            center(),
            objc::sel::get("addObserver:selector:name:object:"),
            observer,
            objc::sel::get("heard:"),
            name,
            std::ptr::null_mut(),
        );
    }
}

fn post(name: Id) {
    // SAFETY: `-postNotificationName:object:` is `void (id, SEL, id, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute(objc::msg::entry());
        f(
            center(),
            objc::sel::get("postNotificationName:object:"),
            name,
            std::ptr::null_mut(),
        );
    }
}

fn deallocated_observer_is_not_notified() {
    objc::debug::enable();
    foundation::bootstrap();
    let pool = objc::arc::pool_push();
    let superclass = objc::class::lookup(c"NSObject").expect("NSObject");
    let class = corefoundation::base::register(
        c"RosemaryTestForgetfulObserver",
        Some(superclass),
        size_of::<usize>(),
        false,
    );
    class.replace_method(
        objc::sel::get("heard:"),
        heard as *const std::ffi::c_void,
        c"v24@0:8@16",
    );
    let name = foundation::string::from_str(NAME);

    let observer = send0(objc::class::ptr(class) as Id, "new");
    add_observer(observer, name);
    post(name);
    assert_eq!(
        HEARD.load(Ordering::SeqCst),
        1,
        "a live observer missed the post"
    );

    // Gone without `-removeObserver:`, which a 10.11 program may do.
    objc::arc::release(observer);
    post(name);
    assert_eq!(
        HEARD.load(Ordering::SeqCst),
        1,
        "a freed observer was sent the post"
    );

    objc::arc::release(name);
    objc::arc::pool_pop(pool);
}

#[test]
fn a_deallocated_observer_is_not_notified() {
    if std::env::var_os(CHILD).is_some() {
        deallocated_observer_is_not_notified();
        return;
    }
    // A child process, so the debug checks' quarantine catches a send to the
    // freed observer without turning them on for every other test.
    let output = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .args([
            "--exact",
            "a_deallocated_observer_is_not_notified",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .expect("run the debug-checked child");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
