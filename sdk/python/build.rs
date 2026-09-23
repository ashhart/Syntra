// Lets `cargo build` produce a loadable extension module on macOS
// (`-undefined dynamic_lookup`); maturin does the same when it builds wheels.
fn main() {
    pyo3_build_config::add_extension_module_link_args();
}
