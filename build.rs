// Stages the Noesis runtime library so this crate's examples and tests run
// without manual setup. noesis_runtime's build script publishes the resolved
// Bin/<platform> path as DEP_NOESIS_LIB_DIR (`links = "Noesis"`).
//
// Linux bakes that path into the rpath. Windows has no rpath, so Noesis.dll is
// copied next to the binaries instead. noesis_runtime stages the same DLL for
// its own build; repeating it here covers this crate's incremental rebuilds.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=DEP_NOESIS_LIB_DIR");

    // No SDK under DOCS_RS: noesis_runtime returns before emitting DEP_NOESIS_LIB_DIR.
    if env::var_os("DOCS_RS").is_some() {
        return;
    }

    let lib_dir = env::var("DEP_NOESIS_LIB_DIR").expect(
        "DEP_NOESIS_LIB_DIR not set: noesis_runtime's build.rs should emit it via \
         `cargo:lib_dir=...`. Did the noesis_runtime dependency build?",
    );

    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir}");
        }
        Ok("windows") => {
            // OUT_DIR is <target>/<profile>/build/<pkg>-<hash>/out; three levels
            // up is the profile dir holding the binaries, deps/ and examples/.
            let dll = Path::new(&lib_dir).join("Noesis.dll");
            let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
            if let Some(profile_dir) = out_dir.ancestors().nth(3) {
                for sub in ["", "deps", "examples"] {
                    let dest = profile_dir.join(sub);
                    if dest.is_dir() {
                        // Best effort: PATH remains a fallback.
                        let _ = std::fs::copy(&dll, dest.join("Noesis.dll"));
                    }
                }
            }
        }
        _ => {}
    }
}
