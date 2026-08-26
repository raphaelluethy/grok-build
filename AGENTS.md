# Build

Whenever you build this repository's CLI/release binary, use `./build-grog.sh`. Do not invoke the release Cargo build directly.

The script builds upstream `xai-grok-pager` and refreshes the personal fork command at `target/release/grog`.

Targeted `cargo check` and `cargo test` remain allowed for development verification. The final runnable build must go through the script.
