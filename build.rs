fn main() {
    prost_build::compile_protos(&["proto/veyron_protocol.proto"], &["proto/"])
        .unwrap_or_else(|e| panic!("proto codegen failed: {}", e));
}
