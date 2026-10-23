# Contributing

Contributions are welcome. Please feel free to submit issues and pull requests.

## Getting Started

1. Fork the repository
2. Clone your fork
3. Create a new branch for your changes
4. Make your changes
5. Run tests to ensure everything passes
6. Submit a pull request

## Development

### Prerequisites

- Rust 1.85.0 or later (see `rust-version` in Cargo.toml)
- For integration tests: PostgreSQL (optional)

### Building

```bash
cargo build
```

### Testing

```bash
cargo test
cargo test --features axum
```

### Running the checker

```bash
cargo run --bin seaorm-auto-audit-check -- <path>
```

## Guidelines

- Follow Rust conventions and style (use `cargo fmt`)
- Add tests for new functionality
- Update documentation as needed
- Keep changes focused and atomic

## Code of Conduct

Be respectful and constructive in all interactions.
