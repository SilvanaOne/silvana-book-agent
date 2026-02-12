//! Decode Canton `PreparedTransaction` protobuf to JSON
//!
//! Uses prost-generated types compiled from the Canton interactive submission protos.

use anyhow::{Context, Result};
use prost::Message;

/// Generated Canton proto types
#[allow(clippy::all)]
pub mod canton_proto {
    pub mod google {
        pub mod protobuf {
            include!(concat!(env!("OUT_DIR"), "/google.protobuf.rs"));
        }
        pub mod rpc {
            include!(concat!(env!("OUT_DIR"), "/google.rpc.rs"));
        }
    }

    pub mod com {
        pub mod daml {
            pub mod ledger {
                pub mod api {
                    pub mod v2 {
                        include!(concat!(env!("OUT_DIR"), "/com.daml.ledger.api.v2.rs"));

                        pub mod interactive {
                            include!(concat!(
                                env!("OUT_DIR"),
                                "/com.daml.ledger.api.v2.interactive.rs"
                            ));

                            pub mod transaction {
                                pub mod v1 {
                                    include!(concat!(
                                        env!("OUT_DIR"),
                                        "/com.daml.ledger.api.v2.interactive.transaction.v1.rs"
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

use canton_proto::com::daml::ledger::api::v2::interactive::PreparedTransaction;

/// Decode `PreparedTransaction` protobuf bytes and return pretty-printed JSON.
pub fn decode_prepared_transaction_json(bytes: &[u8]) -> Result<String> {
    let prepared = PreparedTransaction::decode(bytes)
        .context("Failed to decode PreparedTransaction protobuf")?;
    serde_json::to_string_pretty(&prepared)
        .context("Failed to serialize PreparedTransaction to JSON")
}
