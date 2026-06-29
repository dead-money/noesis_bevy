// Re-emit the libNoesis rpath for binaries linked from this crate (examples,
// integration tests). Cargo's `cargo:rustc-link-arg` from noesis_runtime's build.rs
// only applies to its OWN binaries, not to downstream consumers, so without
// this, `cargo run --example hello_xaml` builds fine but fails at runtime with
// "libNoesis.so: cannot open shared object file".
//
// noesis_runtime publishes the resolved Bin/<platform> path as DEP_NOESIS_LIB_DIR
// (via `cargo:lib_dir=` + `links = "Noesis"` in its manifest). We reuse it.

fn main() {
    println!("cargo:rerun-if-env-changed=DEP_NOESIS_LIB_DIR");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        // Windows: Noesis.dll must sit next to the .exe, not via rpath.
        return;
    }

    let lib_dir = std::env::var("DEP_NOESIS_LIB_DIR").expect(
        "DEP_NOESIS_LIB_DIR not set — noesis_runtime's build.rs should emit it via \
         `cargo:lib_dir=...`. Did the noesis_runtime dependency build?",
    );
    println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir}");
}
