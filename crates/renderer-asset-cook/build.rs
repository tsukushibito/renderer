use std::path::Path;

fn main() {
    let mikk = Path::new("../../tools/bistro/mikktspace");
    for file in ["bridge.c", "mikktspace.c", "mikktspace.h"] {
        println!("cargo:rerun-if-changed={}", mikk.join(file).display());
    }
    cc::Build::new()
        .include(mikk)
        .files([mikk.join("bridge.c"), mikk.join("mikktspace.c")])
        .warnings(false)
        .compile("asset_mikktspace");
}
