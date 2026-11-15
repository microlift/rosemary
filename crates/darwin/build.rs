fn main() {
    println!("cargo:rerun-if-changed=src/fortify.c");
    println!("cargo:rerun-if-changed=src/sprintf.c");

    // Hosts where Rosemary puts the Darwin TSD array in the register Darwin code
    // reads it from, beside the host's own thread pointer: `%gs` on x86-64, and
    // `TPIDRRO_EL0` on aarch64 Linux through `kernel/linux`.
    println!("cargo::rustc-check-cfg=cfg(installs_tsd_register)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if matches!(
        (os.as_str(), arch.as_str()),
        ("linux" | "freebsd", "x86_64") | ("linux", "aarch64")
    ) {
        println!("cargo::rustc-cfg=installs_tsd_register");
    }

    cc::Build::new()
        .file("src/fortify.c")
        .file("src/sprintf.c")
        .compile("libdarwin.a");
}
