fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = prost_build::Config::new();

    // Generate our own google.protobuf types with serde::Serialize
    // (prost_types doesn't implement Serialize)
    config.compile_well_known_types();
    config.extern_path(".google.protobuf.BoolValue", "bool");

    config.type_attribute(".", "#[derive(serde::Serialize)]");

    config.compile_protos(
        &["proto/com/daml/ledger/api/v2/interactive/interactive_submission_service.proto"],
        &["proto"],
    )?;
    println!("cargo:rerun-if-changed=proto/");
    Ok(())
}
