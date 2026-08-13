# Contributing

Please see **[Contributing on stateroot.dev](https://stateroot.dev/docs/developer-guide/contributing)**.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Please preserve [product intent](https://stateroot.dev/docs/features/rules): do not replace agent judgment with classifiers, or truncate identity to look conservative.
