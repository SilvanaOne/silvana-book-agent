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
    let proto_includes = vec!["../../proto"];

    // ledger.proto has been split into per-domain files; all share the
    // silvana.ledger.v1 package and compile into a single descriptor pool.
    // bridge.proto cross-imports silvana/rfqv2/v1/atomic_ledger.proto (rev 3:
    // the shared tunnel carries the atomic RPCs), so the silvana.rfqv2.v1
    // files MUST be compiled in the same unit — prost emits relative
    // super::…::rfqv2::v1 paths in the generated ledger code.
    let ledger_proto_dir = "../../proto/silvana/ledger/v1";
    let rfqv2_proto_dir = "../../proto/silvana/rfqv2/v1";
    let mut ledger_protos: Vec<String> = [
        "common.proto",
        "queries.proto",
        "onboarding.proto",
        "transfer.proto",
        "preapproval.proto",
        "dvp.proto",
        "cip56.proto",
        "recurring.proto",
        "multicall.proto",
        "fees.proto",
        "pay_fee.proto",
        "transactions.proto",
        "service.proto",
        "bridge.proto",
    ]
    .iter()
    .map(|f| format!("{}/{}", ledger_proto_dir, f))
    .collect();
    ledger_protos.extend(
        ["rfqv2.proto", "atomic_ledger.proto"]
            .iter()
            .map(|f| format!("{}/{}", rfqv2_proto_dir, f)),
    );
    // tonic-prost-build's compile_with_config requires both `protos` and
    // `includes` slices to use the same generic type — so for the ledger
    // call (which uses Vec<String>) we need a String-typed include list.
    let ledger_proto_includes: Vec<String> = proto_includes.iter().map(|s| s.to_string()).collect();

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

    // Configure for ledger + rfqv2 protos (one compile unit; see note above)
    let mut config_ledger = Config::new();
    config_ledger.protoc_arg("--experimental_allow_proto3_optional");

    // Enable prost-reflect for runtime reflection
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::LEDGER_DESCRIPTOR_POOL")
        .configure(
            &mut config_ledger,
            &ledger_protos,
            &ledger_proto_includes
        )
        .expect("Failed to configure reflection for ledger proto");

    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .build_server(false)  // Client only
        .build_client(true)
        .file_descriptor_set_path(&ledger_descriptor_path)
        .compile_with_config(
            config_ledger,
            &ledger_protos,
            &ledger_proto_includes
        )?;

    // Tell cargo to recompile if any .proto files change
    println!("cargo:rerun-if-changed=../../proto/silvana/orderbook/");
    println!("cargo:rerun-if-changed=../../proto/silvana/settlement/");
    println!("cargo:rerun-if-changed=../../proto/silvana/pricing/");
    println!("cargo:rerun-if-changed=../../proto/silvana/ledger/");
    println!("cargo:rerun-if-changed=../../proto/silvana/rfqv2/");

    Ok(())
}
