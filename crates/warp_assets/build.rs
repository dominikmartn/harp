// rust-embed only tracks the asset files that existed when this crate last compiled, so a newly
// added asset would stay missing from the binary; rescanning the folder forces a rebuild.
fn main() {
    println!("cargo:rerun-if-changed=../../app/assets");
}
