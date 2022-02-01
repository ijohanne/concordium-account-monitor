use std::env;

fn main() -> std::io::Result<()> {
    let cargo_dir = env!("CARGO_MANIFEST_DIR");
    let proto_root_input = format!("{}/proto", cargo_dir);
    let proto = format!("{}/concordium.proto", proto_root_input);
    println!("cargo:rerun-if-changed={}", proto);
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile(&[&proto], &[&proto_root_input])
        .expect("Failed to compile gRPC definitions!");
    Ok(())
}
