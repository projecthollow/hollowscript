//! The MSVC linker cannot infer a subsystem from Rust's `no_main` objects.
//! Select the console subsystem for libFuzzer's C main, so the CRT entry
//! point pulls it from the static library. Ordinary Rust tools keep their
//! own entry points and linker defaults.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        for target in [
            "lex_parse",
            "format_roundtrip",
            "check",
            "pipeline",
            "bytecode_verify",
            "capi",
            "capi_ex",
            "wasm_abi",
            "filter",
        ] {
            println!("cargo:rustc-link-arg-bin={target}=/SUBSYSTEM:CONSOLE");
        }
    }
}
