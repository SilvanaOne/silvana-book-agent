use prost_build::Config;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let orderbook_descriptor_path =
        PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("orderbook_descriptor.bin");
    let settlement_descriptor_path =
        PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("settlement_descriptor.bin");
    let pricing_descriptor_path =
        PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("pricing_descriptor.bin");
    let ledger_descriptor_path =
        PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("ledger_descriptor.bin");

    // Proto file paths
    let orderbook_proto = "../../proto/silvana/orderbook/v1/orderbook.proto";
    let settlement_proto = "../../proto/silvana/settlement/v1/settlement.proto";
    let pricing_proto = "../../proto/silvana/pricing/v1/pricing.proto";
    let ledger_proto = "../../proto/silvana/ledger/v1/ledger.proto";
    let proto_includes = vec!["../../proto"];

    // Configure for orderbook.proto
    let mut config_orderbook = Config::new();
    config_orderbook.protoc_arg("--experimental_allow_proto3_optional");

    // Enable prost-reflect for runtime reflection
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::ORDERBOOK_DESCRIPTOR_POOL")
        .configure(
            &mut config_orderbook,
            &[orderbook_proto],
            &proto_includes
        )
        .expect("Failed to configure reflection for orderbook proto");

    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .build_server(false)  // Client only
        .build_client(true)
        .file_descriptor_set_path(&orderbook_descriptor_path)
        .compile_with_config(
            config_orderbook,
            &[orderbook_proto],
            &proto_includes
        )?;

    // Configure for settlement.proto
    let mut config_settlement = Config::new();
    config_settlement.protoc_arg("--experimental_allow_proto3_optional");

    // Enable prost-reflect for runtime reflection
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::SETTLEMENT_DESCRIPTOR_POOL")
        .configure(
            &mut config_settlement,
            &[settlement_proto],
            &proto_includes
        )
        .expect("Failed to configure reflection for settlement proto");

    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .build_server(false)  // Client only
        .build_client(true)
        .file_descriptor_set_path(&settlement_descriptor_path)
        .compile_with_config(
            config_settlement,
            &[settlement_proto],
            &proto_includes
        )?;

    // Configure for pricing.proto
    let mut config_pricing = Config::new();
    config_pricing.protoc_arg("--experimental_allow_proto3_optional");

    // Enable prost-reflect for runtime reflection
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::PRICING_DESCRIPTOR_POOL")
        .configure(
            &mut config_pricing,
            &[pricing_proto],
            &proto_includes
        )
        .expect("Failed to configure reflection for pricing proto");

    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .build_server(false)  // Client only
        .build_client(true)
        .file_descriptor_set_path(&pricing_descriptor_path)
        .compile_with_config(
            config_pricing,
            &[pricing_proto],
            &proto_includes
        )?;

    // Configure for ledger.proto
    let mut config_ledger = Config::new();
    config_ledger.protoc_arg("--experimental_allow_proto3_optional");

    // Enable prost-reflect for runtime reflection
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::LEDGER_DESCRIPTOR_POOL")
        .configure(
            &mut config_ledger,
            &[ledger_proto],
            &proto_includes
        )
        .expect("Failed to configure reflection for ledger proto");

    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .build_server(false)  // Client only
        .build_client(true)
        .file_descriptor_set_path(&ledger_descriptor_path)
        .compile_with_config(
            config_ledger,
            &[ledger_proto],
            &proto_includes
        )?;

    // Tell cargo to recompile if any .proto files change
    println!("cargo:rerun-if-changed=../../proto/silvana/orderbook/");
    println!("cargo:rerun-if-changed=../../proto/silvana/settlement/");
    println!("cargo:rerun-if-changed=../../proto/silvana/pricing/");
    println!("cargo:rerun-if-changed=../../proto/silvana/ledger/");

    Ok(())
}
