// Built with plain `cargo build` (no maturin), so the macOS extension-module
// link args (-undefined dynamic_lookup) must be added here ourselves.
fn main() {
    pyo3_build_config::add_extension_module_link_args();
}
