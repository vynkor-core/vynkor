use std::env;
use prost_build::Config;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let proto_file = "veyron_protocol.proto";
    Config::new()
        .out_dir(&out_dir)
        .compile_protos(&[proto_file], &["."])
        .unwrap_or_else(|e| panic!("Failed to compile protos: {}", e));
    println!("cargo:rustc-env=PROTO_OUT_DIR={}", out_dir);
}
