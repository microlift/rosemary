use libc::c_int;
use std::sync::LazyLock;

unsafe extern "C" fn res_9_init() -> c_int {
    static RES_INIT_SYM: LazyLock<usize> = LazyLock::new(|| {
        let lresolv_names = [c"libresolv.so", c"libresolv.dylib"];
        for resolv in &lresolv_names {
            unsafe { libc::dlopen(resolv.as_ptr(), libc::RTLD_LAZY) };
        }
        unsafe {
            let sym = libc::dlsym(libc::RTLD_DEFAULT, c"res_init".as_ptr());
            sym as usize
        }
    });
    if *RES_INIT_SYM != 0 {
        unsafe {
            let res_init: unsafe extern "C" fn() -> c_int = std::mem::transmute(*RES_INIT_SYM);
            res_init()
        }
    } else {
        // FIXME (ACCURACY): does nothing if res_init is not found
        0
    }
}

unsafe extern "C" fn res_9_close() {
    // FIXME (ACCURACY): unimplemented!
}

exports::exports! {
    functions {
        "res_9_init" => res_9_init,
        "res_9_close" => res_9_close,
    }
}
