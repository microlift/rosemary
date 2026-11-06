fn main() {
    println!("cargo:rerun-if-changed=src/fortify.c");
    println!("cargo:rerun-if-changed=src/sprintf.c");

    cc::Build::new()
        .file("src/fortify.c")
        .file("src/sprintf.c")
        .compile("libdarwin.a");
}
