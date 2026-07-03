# Agent Guidelines for Rust Code Quality

This document provides guidelines for maintaining high-quality Rust code. These rules MUST be followed by all AI coding agents and contributors.

## Your Core Principles

All code you write MUST be fully optimized.

"Fully optimized" includes:

- maximizing algorithmic big-O efficiency for memory and runtime
- using parallelization and SIMD where appropriate
- following proper style conventions for Rust (e.g. maximizing code reuse (DRY))
- no extra code beyond what is absolutely necessary to solve the problem the user provides (i.e. no technical debt)

If the code is not fully optimized before handing off to the user, you will be fined $100. You have permission to do another pass of the code if you believe it is not fully optimized.

## Preferred Tools

- Use `cargo` for project management, building, and dependency management.
- Use `indicatif` to track long-running operations with progress bars. The message should be contextually sensitive.
- Use `serde` with `serde_json` for JSON serialization/deserialization.
- Use `ratatui` adnd `crossterm` for terminal applications/TUIs.
- Use `axum` for creating any web servers or HTTP APIs.
    - Keep request handlers async, returning `Result<Response, AppError>` to centralize error handling.
    - Use layered extractors and shared state structs instead of global mutable data.
    - Add `tower` middleware (timeouts, tracing, compression) for observability and resilience.
    - Offload CPU-bound work to `tokio::task::spawn_blocking` or background services to avoid blocking the reactor.
- When reporting errors to the console, use `tracing::error!` or `log::error!` instead of `println!`.
- For data processing:
    - **ALWAYS** use `polars` instead of other data frame libraries for tabular data manipulation.
    - If a `polars` dataframe will be printed, **NEVER** simultaneously print the number of entries in the dataframe nor the schema as it is redundant.
    - **NEVER** ingest more than 10 rows of a data frame at a time. Only analyze subsets of data to avoid overloading your memory context.

## Code Style and Formatting

- **MUST** use meaningful, descriptive variable and function names
- **MUST** follow Rust API Guidelines and idiomatic Rust conventions
- **MUST** use 4 spaces for indentation (never tabs)
- **NEVER** use emoji, or unicode that emulates emoji (e.g. ✓, ✗). The only exception is when writing tests and testing the impact of multibyte characters.
- Use snake_case for functions/variables/modules, PascalCase for types/traits, SCREAMING_SNAKE_CASE for constants
- Limit line length to 100 characters (rustfmt default)

## Documentation

- **MUST** include doc comments for all public functions, structs, enums, and methods
- **MUST** document function parameters, return values, and errors
- Keep comments up-to-date with code changes
- Include examples in doc comments for complex functions

Example doc comment:

````rust
/// Calculate the total cost of items including tax.
///
/// # Arguments
///
/// * `items` - Slice of item structs with price fields
/// * `tax_rate` - Tax rate as decimal (e.g., 0.08 for 8%)
///
/// # Returns
///
/// Total cost including tax
///
/// # Errors
///
/// Returns `CalculationError::EmptyItems` if items is empty
/// Returns `CalculationError::InvalidTaxRate` if tax_rate is negative
///
/// # Examples
///
/// ```
/// let items = vec![Item { price: 10.0 }, Item { price: 20.0 }];
/// let total = calculate_total(&items, 0.08)?;
/// assert_eq!(total, 32.40);
/// ```
pub fn calculate_total(items: &[Item], tax_rate: f64) -> Result<f64, CalculationError> {
````

## Type System

- **MUST** leverage Rust's type system to prevent bugs at compile time
- **NEVER** use `.unwrap()` in library code; use `.expect()` only for invariant violations with a descriptive message
- **MUST** use meaningful custom error types with `thiserror`
- Use newtypes to distinguish semantically different values of the same underlying type
- Prefer `Option<T>` over sentinel values

## Error Handling

- **NEVER** use `.unwrap()` in production code paths
- **MUST** use `Result<T, E>` for fallible operations
- **MUST** use `thiserror` for defining error types and `anyhow` for application-level errors
- **MUST** propagate errors with `?` operator where appropriate
- Provide meaningful error messages with context using `.context()` from `anyhow`

## Function Design

- **MUST** keep functions focused on a single responsibility
- **MUST** prefer borrowing (`&T`, `&mut T`) over ownership when possible
- Limit function parameters to 5 or fewer; use a config struct for more
- Return early to reduce nesting
- Use iterators and combinators over explicit loops where clearer

### Write Short Functions

Prefer small and focused functions.

**MUST** keep every function under **1000 lines**. This is a hard upper bound, not a target. If a function exceeds about 40 lines, think about whether it can be broken up without harming the structure of the program; long before approaching the 1000-line ceiling you should already have split the function into helpers.

Even if your long function works perfectly now, someone modifying it in a few months may add new behavior. This could result in bugs that are hard to find. Keeping your functions short and simple makes it easier for other people to read and modify your code. Small functions are also easier to test.

You could find long and complicated functions when working with some code. Do not be intimidated by modifying existing code: if working with such a function proves to be difficult, you find that errors are hard to debug, or you want to use a piece of it in several different contexts, consider breaking up the function into smaller and more manageable pieces.

### Keep Files Small

**MUST** keep every source file under **5000 lines**. This is a hard upper bound. When a file approaches the limit, split it into a module directory (`foo.rs` → `foo/mod.rs` plus feature-scoped submodules) before adding more code. Tests count toward the limit; large `#[cfg(test)] mod tests` blocks should be moved into a sibling `tests.rs` submodule.

## Struct and Enum Design

- **MUST** keep types focused on a single responsibility
- **MUST** derive common traits: `Debug`, `Clone`, `PartialEq` where appropriate
- Use `#[derive(Default)]` when a sensible default exists
- Prefer composition over inheritance-like patterns
- Use builder pattern for complex struct construction
- Make fields private by default; provide accessor methods when needed

## Testing

- **MUST** write unit tests for all new functions and types
- **MUST** mock external dependencies (APIs, databases, file systems)
- **MUST** use the built-in `#[test]` attribute and `cargo test`
- Follow the Arrange-Act-Assert pattern
- Do not commit commented-out tests
- Use `#[cfg(test)]` modules for test code

## Code Coverage

- **MUST** use `cargo llvm-cov` (`cargo-llvm-cov`) to measure coverage
- Generate an HTML report with `cargo llvm-cov --workspace --html`; the report is written to `target/llvm-cov/html/index.html`
- Every new public function or behaviour change **MUST** be covered by at least one test; aim to keep line coverage above **80%** across the workspace
- Cover both the happy path and key error/edge-case branches
- Do not add `#[allow(dead_code)]` or dummy call sites solely to satisfy the coverage tool; fix the underlying gap with a real test
- Add coverage generation to the pre-commit checklist (see below)

## Imports and Dependencies

- **MUST** avoid wildcard imports (`use module::*`) except for preludes, test modules (`use super::*`), and prelude re-exports
- **MUST** document dependencies in `Cargo.toml` with version constraints
- Use `cargo` for dependency management
- Organize imports: standard library, external crates, local modules
- Use `rustfmt` to automate import formatting

## Rust Best Practices

- **NEVER** use `unsafe` unless absolutely necessary; document safety invariants when used
- **MUST** call `.clone()` explicitly on non-`Copy` types; avoid hidden clones in closures and iterators
- **MUST** declare parameterless `&'static str` (or any other pure-constant) producers as `const`, not `fn`. Anything shaped like `fn FOO() -> &'static str { "..." }` should be `const FOO: &str = "...";` — call sites become `FOO` instead of `FOO()`. The constant form makes it immediately obvious there is no runtime work; it can be used in `const` contexts; and it costs zero indirection. The only reason to keep a function is when the value actually depends on a runtime input. This applies most often to hand-written SQL string helpers.
- **MUST** use pattern matching exhaustively; avoid catch-all `_` patterns when possible
- **MUST** use `format!` macro for string formatting
- Use iterators and iterator adapters over manual loops
- Use `enumerate()` instead of manual counter variables
- Prefer `if let` and `while let` for single-pattern matching

## Memory and Performance

- **MUST** avoid unnecessary allocations; prefer `&str` over `String` when possible
- **MUST** use `Cow<'_, str>` when ownership is conditionally needed
- Use `Vec::with_capacity()` when the size is known
- Prefer stack allocation over heap when appropriate
- Use `Arc` and `Rc` judiciously; prefer borrowing

## Concurrency

- **MUST** use `Send` and `Sync` bounds appropriately
- **MUST** prefer `tokio` for async runtime in async applications
- **MUST** use `rayon` for CPU-bound parallelism
- Avoid `Mutex` when `RwLock` or lock-free alternatives are appropriate
- Use channels (`mpsc`, `crossbeam`) for message passing

## Security

- **NEVER** store secrets, API keys, or passwords in code. Only store them in `.env`.
    - Ensure `.env` is declared in `.gitignore`.
- **MUST** use environment variables for sensitive configuration via `dotenvy` or `std::env`
- **NEVER** log sensitive information (passwords, tokens, PII)
- Use `secrecy` crate for sensitive data types

## Version Control

- **MUST** write clear, descriptive commit messages
- **NEVER** commit commented-out code; delete it
- **NEVER** commit debug `println!` statements or `dbg!` macros
- **NEVER** commit credentials or sensitive data

## Tools

- **MUST** use `rustfmt` for code formatting
- **MUST** use `clippy` for linting and follow its suggestions
- **MUST** ensure code compiles with no warnings (use `-D warnings` flag in CI, not `#![deny(warnings)]` in source)
- Use `cargo` for building, testing, and dependency management
- Use `cargo test` for running tests
- Use `cargo doc` for generating documentation
- **NEVER** build with `cargo build --features python`: this will always fail. Instead, **ALWAYS** use `maturin`.

## Before Committing

- [ ] All tests pass (`cargo test`)
- [ ] No compiler warnings (`cargo build`)
- [ ] Clippy passes (`cargo clippy -- -D warnings`)
- [ ] Code is formatted (`cargo fmt --check`)
- [ ] All public items have doc comments
- [ ] No commented-out code or debug statements
- [ ] No hardcoded credentials
- [ ] Coverage generated and reviewed (`cargo llvm-cov --workspace --html`); new code is covered

---

**Remember:** Prioritize clarity and maintainability over cleverness.
