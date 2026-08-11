mod helpers;
#[path = "../support/jwt_helper.rs"]
mod jwt_helper;
mod sdk_harness;
mod test_audio_stream_permission;
mod test_autoload;
mod test_disconnect;
mod test_event_store_integration;
mod test_events;
mod test_kernel_commands;
mod test_mac;
mod test_metrics_counters;
mod test_ping;
mod test_registration;
mod test_routing;
mod test_sdk_cpp;
mod test_sdk_python;
mod test_sdk_rust;
#[cfg(target_os = "linux")]
mod test_shim;
mod test_sigterm;
mod test_soak;
mod test_websocket;
