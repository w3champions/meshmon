use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    // SAFETY: build scripts run single-threaded before Cargo spawns the compiler.
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    tonic_prost_build::configure()
        .bytes(".")
        .compile_protos(&["proto/meshmon.proto"], &["proto"])?;

    println!("cargo:rerun-if-changed=proto/meshmon.proto");
    println!("cargo:rerun-if-changed=build.rs");

    Ok(())
}
