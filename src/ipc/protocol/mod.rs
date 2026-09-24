pub(crate) mod helpers;
pub(crate) mod router;

// Re-export MessageRouter for backward compatibility
pub use router::MessageRouter;
// Re-export helpers used by other modules (e.g. events/bus.rs)
pub(crate) use helpers::kernel_message_id;

/// CD-06: oldest wire protocol this kernel still accepts. Kernel policy, not
/// a wire constant — the wire crate only knows its own (newest) version
pub const MIN_SUPPORTED_PROTOCOL_VERSION: &str = "1.5";
