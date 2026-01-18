#!/bin/bash
# Test script

set -e

echo "Running tests..."

# Unit tests
cargo test --lib --all-features

# Integration tests (when they exist)
# cargo test --test '*' --all-features

# Doc tests
cargo test --doc --all-features

echo "✓ All tests passed"
