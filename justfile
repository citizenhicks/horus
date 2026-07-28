test *args:
    cargo test --workspace --all-targets --all-features --locked {{args}}

fmt:
    cargo fmt
