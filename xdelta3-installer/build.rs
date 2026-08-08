use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_DLL_REL: &str = "i686-pc-windows-msvc/release/xdelta3_wrap.dll";
const DEFAULT_XDELTA_REL: &str = "x86_64-pc-windows-msvc/release/xdelta.exe";

fn main() {
    println!("cargo:rerun-if-env-changed=XDELTA3_WRAP_DLL");
    println!("cargo:rerun-if-env-changed=XDELTA_EXE");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let workspace = env::var("CARGO_WORKSPACE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
                .parent()
                .unwrap()
                .to_path_buf()
        });

    let dll = resolve("XDELTA3_WRAP_DLL", &workspace, DEFAULT_DLL_REL);
    let xdelta = resolve("XDELTA_EXE", &workspace, DEFAULT_XDELTA_REL);

    if dll.is_none() {
        eprintln!("note: xdelta3_wrap.dll not found; installer will fall back to --dll or an exe-adjacent file");
    }
    if xdelta.is_none() {
        eprintln!("note: xdelta.exe not found; installer will fall back to --xdelta or an exe-adjacent file");
    }

    let embedded = format!(
        "pub static DLL: &[u8] = {};\npub static XDELTA: &[u8] = {};\n",
        bytes_literal(dll.as_deref()),
        bytes_literal(xdelta.as_deref()),
    );
    fs::write(out_dir.join("embedded.rs"), embedded).unwrap();
}

fn resolve(env_key: &str, workspace: &Path, default_rel: &str) -> Option<PathBuf> {
    if let Ok(v) = env::var(env_key) {
        let p = PathBuf::from(&v);
        if p.is_file() {
            println!("cargo:rerun-if-changed={}", p.display());
            return Some(p);
        }
        eprintln!("warning: {env_key}={v} does not point at an existing file; ignoring it");
    }
    let p = workspace.join("target").join(default_rel);
    // Watch the default path even when absent, so a later artifact build
    // triggers re-embedding.
    println!("cargo:rerun-if-changed={}", p.display());
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn bytes_literal(p: Option<&Path>) -> String {
    match p {
        // `{:?}` of a string yields a quoted, backslash-escaped Rust literal.
        Some(p) => format!("include_bytes!({:?})", p.to_string_lossy()),
        None => "&[]".to_string(),
    }
}
