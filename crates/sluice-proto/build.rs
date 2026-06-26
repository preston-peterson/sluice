//! Compile `proto/sluice.proto` into Rust gRPC stubs (server + client) at build time.
//! Requires `protoc` on PATH (already a documented build prerequisite).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = "proto/sluice.proto";
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&[proto], &["proto"])?;
    println!("cargo:rerun-if-changed={proto}");
    Ok(())
}
