// Built with plain `cargo build` (no maturin), so the macOS extension-module
// link args (-undefined dynamic_lookup) must be added here ourselves.
fn main() {
    // Without a rerun line cargo reruns this script, and rebuilds the crate,
    // whenever any file in the package folder changes, and that folder is the
    // whole of AutoCua/ (every .py, every prompt).
    println!("cargo:rerun-if-changed=build.rs");
    pyo3_build_config::add_extension_module_link_args();
}
