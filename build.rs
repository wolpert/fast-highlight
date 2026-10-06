//! Generates the table of built-in command specs from the contents of `specs/`.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=specs");

    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let specs_dir = manifest_dir.join("specs");

    let entries = std::fs::read_dir(&specs_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", specs_dir.display()));
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry
            .unwrap_or_else(|e| panic!("cannot read an entry of {}: {e}", specs_dir.display()));
        let path = entry.path();
        let name = entry.file_name().into_string().unwrap_or_else(|name| {
            panic!(
                "file name is not valid UTF-8: {}",
                PathBuf::from(name).display()
            )
        });
        let meta = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        if meta.is_file() && path.extension().is_some_and(|ext| ext == "toml") {
            names.push(name);
        }
    }
    names.sort_unstable();

    let mut out = String::from("&[\n");
    for name in &names {
        writeln!(
            out,
            "    ({name:?}, include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/specs/\", {name:?}))),"
        )
        .expect("write to String");
    }
    out.push_str("]\n");

    let dest = out_dir.join("builtin_specs.rs");
    std::fs::write(&dest, out).unwrap_or_else(|e| panic!("cannot write {}: {e}", dest.display()));
}
