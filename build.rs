fn main() {
    println!(
        "cargo:rustc-env=CLIENTPROXY_BUILD_TARGET={}",
        std::env::var("TARGET").expect("Cargo target")
    );
    println!("cargo:rerun-if-changed=build.rs");
}
