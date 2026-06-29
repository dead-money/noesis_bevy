// Stage the Noesis runtime library so binaries linked from this crate (examples,
// integration tests) find it without manual setup. noesis_runtime's build.rs
// links Noesis and publishes the resolved Bin/<platform> path as
// DEP_NOESIS_LIB_DIR (via `cargo:lib_dir=` + `links = "Noesis"`); we reuse it.
//
// Linux bakes that path into the binary's rpath so libNoesis.so loads without
// LD_LIBRARY_PATH. Windows has no rpath: copy Noesis.dll next to the binaries so
// the loader finds it, the parity of the rpath. (noesis_runtime stages the same
// DLL into the dependency's build; doing it here too covers this crate's own
// incremental rebuilds.)

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=DEP_NOESIS_LIB_DIR");

    // docs.rs (and the `doc` CI job) build with no Noesis SDK. noesis_runtime's
    // build.rs short-circuits on DOCS_RS before it emits DEP_NOESIS_LIB_DIR, so
    // there's nothing to stage. Skip.
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
            // The loader finds Noesis.dll next to the .exe or on PATH. Copy it
            // beside this crate's test and example binaries so they run straight
            // from `cargo test` / `cargo run`. OUT_DIR is
            // <target>/<profile>/build/<pkg>-<hash>/out; the profile dir three
            // levels up holds the binaries and their deps/ and examples/.
            let dll = Path::new(&lib_dir).join("Noesis.dll");
            let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
            if let Some(profile_dir) = out_dir.ancestors().nth(3) {
                for sub in ["", "deps", "examples"] {
                    let dest = profile_dir.join(sub);
                    if dest.is_dir() {
                        // Best effort: a stale copy or a missing dir is not fatal,
                        // PATH still works as a fallback.
                        let _ = std::fs::copy(&dll, dest.join("Noesis.dll"));
                    }
                }
            }
        }
        _ => {}
    }
}
