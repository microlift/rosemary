#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn chkstk() {
    core::arch::naked_asm!(
        // `rsp` includes this helper's return address; the caller's future
        // stack begins eight bytes above it.
        "lea r10, [rsp + 8]",
        "mov r11, rax",
        "test r11, r11",
        "je 3f",
        "2:",
        "cmp r11, 4096",
        "jbe 3f",
        "sub r10, 4096",
        "test byte ptr [r10], al",
        "sub r11, 4096",
        "jmp 2b",
        "3:",
        "sub r10, r11",
        "test byte ptr [r10], al",
        "ret",
    );
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn chkstk() {
    core::arch::naked_asm!(
        // Darwin arm64 passes the number of bytes to probe in x9, below the
        // current SP. Only x16 and x17 are touched: a caller may still hold
        // the size in x9 to move SP itself, and every call through a stub
        // already gives up the intra-procedure-call registers.
        "mov x16, x9",
        "mov x17, sp",
        "2:",
        "cmp x16, #4096",
        "b.ls 3f",
        "sub x17, x17, #4096",
        "ldr xzr, [x17]",
        "sub x16, x16, #4096",
        "b 2b",
        "3:",
        "sub x17, x17, x16",
        "ldr xzr, [x17]",
        "ret",
    );
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
exports::exports! {
    functions {
        // arm64's compiler calls the first and x86-64's the second; the
        // emulated x86-64 guest reaches its own probe through a shim.
        "__chkstk_darwin" => chkstk,
        "___chkstk_darwin" => chkstk,
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
exports::exports! {}
