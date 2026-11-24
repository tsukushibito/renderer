use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn collect(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in fs::read_dir(path).expect("read converter source directory") {
            collect(&entry.expect("read source entry").path(), files);
        }
    } else {
        files.push(path.to_owned());
    }
}

fn main() {
    let mikk = Path::new("../../tools/bistro/mikktspace");
    cc::Build::new()
        .include(mikk)
        .files([mikk.join("bridge.c"), mikk.join("mikktspace.c")])
        .warnings(false)
        .compile("asset_mikktspace");

    // Scope recipe identity to converter code, native source, shared wire format and pinned
    // tools. Normalize checkout line endings to allow cache reuse across checkout styles.
    let mut files = Vec::new();
    for input in [
        "src",
        "build.rs",
        "Cargo.toml",
        "../renderer-asset/src",
        "../../Cargo.lock",
        "../../tools/bistro/mikktspace/bridge.c",
        "../../tools/bistro/mikktspace/mikktspace.c",
        "../../tools/bistro/mikktspace/mikktspace.h",
    ] {
        println!("cargo:rerun-if-changed={input}");
        collect(Path::new(input), &mut files);
    }
    files.sort();
    let mut hash = Sha256::new();
    for file in files {
        let path = file.to_string_lossy().replace('\\', "/");
        let source = fs::read_to_string(&file)
            .expect("read converter text")
            .replace("\r\n", "\n");
        hash.update((path.len() as u64).to_le_bytes());
        hash.update(path);
        hash.update((source.len() as u64).to_le_bytes());
        hash.update(source);
    }
    println!("cargo:rustc-env=ASSET_COOK_REVISION={:x}", hash.finalize());
}
