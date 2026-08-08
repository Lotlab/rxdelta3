use std::path::PathBuf;

fn main() {
    // Only meaningful for the 32-bit x86 target we build this cdylib for.
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86") {
        return;
    }
    let def = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("exports.def");
    println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def.display());
}
