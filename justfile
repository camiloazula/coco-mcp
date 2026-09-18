# Recipes select packages explicitly with -p. Never use --workspace: the GPUI
# app has heavy native dependencies and must not be built by accident.

set shell := ["bash", "-euo", "pipefail", "-c"]

core := "-p mcp-core -p mcp-schema-form -p mcp-store -p mcp-auth -p mcp-diff -p mcp-exchange -p mcp-mockserver -p coco-cli"
app  := "-p coco-mcp"

# List every recipe (what `just` alone does).
default:
    @just --list --unsorted

# Format, lint, test and audit everything, the app and its headless flows
# included. Run before every commit.
check: fmt-check clippy test clippy-app test-app deny

# Format the whole workspace in place.
fmt:
    cargo fmt --all

# Fail if anything is unformatted.
fmt-check:
    cargo fmt --all -- --check

# Clippy for the UI-free crates and the CLI, warnings denied.
clippy:
    cargo clippy {{core}} --all-targets -- -D warnings

# Clippy for the GPUI app. Slow; run separately.
clippy-app:
    cargo clippy {{app}} --all-targets -- -D warnings

# Unit and integration tests for the UI-free crates and the CLI library.
test:
    cargo test {{core}}

# Unit tests, the command-line mode's end-to-end tests and the headless
# flows of the one binary, with the mock server they spawn built first. Slow.
test-app:
    cargo build -p mcp-mockserver
    cargo test {{app}}

# Build the UI-free crates and the CLI library.
build:
    cargo build {{core}}

# Build the binary.
build-app:
    cargo build {{app}}

# Open the window.
run:
    cargo run {{app}}

# Run the command-line mode, e.g. `just coco snapshot stdio -- some-server --verbose`.
coco *ARGS:
    cargo run {{app}} -- --cli {{ARGS}}

# Run the hermetic mock MCP server, e.g. `just mock --schema v2 --http 127.0.0.1:3555`.
mock *ARGS:
    cargo run -p mcp-mockserver -- {{ARGS}}

# Render the design screens headlessly (macOS) into target/screenshots/, and the README's tour.gif.
screenshots:
    cargo test {{app}} --test screenshots -- --nocapture

# Licence and source audit for the whole dependency graph (metadata only, no build).
deny:
    cargo deny check licenses bans sources

# Vulnerability audit; needs network access to the RustSec database.
audit:
    cargo deny check advisories
