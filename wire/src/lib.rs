pub mod error;
pub mod framing;
pub mod mac;
pub mod socket;
pub mod proto {
    #![allow(clippy::enum_variant_names)]
    pub mod veyron {
        include!(concat!(env!("OUT_DIR"), "/veyron.rs"));
    }
}

pub use error::WireError;
