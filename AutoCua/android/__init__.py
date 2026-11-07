# The Android side: the agent, its tool registry and controller, and the
# phone reader, its part of the one Rust crate (AutoCua/Cargo.toml, lib.rs),
# loaded through AutoCua.android.agent, which builds it on first import in a
# checkout (the pip wheels ship it prebuilt, as agent_native.pyd / .so). The
# connection to the phone lives in AutoCua/android_connector.
