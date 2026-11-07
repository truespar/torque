//! TQBF: the Torque segment format, used both for transport and for serving.
//! Specification: `docs/tqbf-segment-format.md`.

pub mod bytes;
pub mod container;
pub mod docstore;
pub mod dtoa;
pub mod ids;
pub mod json;
pub mod packed;
pub mod record;
pub mod records;
pub mod registry;
pub mod segment;

pub use bytes::Bytes;
pub use registry::FieldRegistry;
