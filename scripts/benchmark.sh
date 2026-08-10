#!/usr/bin/env bash
# Performance benchmark script for forge

set -Eeuo pipefail

echo "📊 forge Performance Benchmark"
echo "================================"
echo ""

# Always build the measured binary from the current sources.
echo "Building release version..."
nix develop --command bash -c "cargo build --release --locked"

# Binary size
echo "📦 Binary Size:"
find target/release -maxdepth 1 -type f -name forge -printf '   %s bytes %p\n'
echo ""

# Headless CLI startup time. This does not claim to measure GTK first-frame time.
echo "⚡ Headless CLI Startup (10 runs):"
total=0
for i in {1..10}; do
    start=$(date +%s%N)
    target/release/forge --version >/dev/null 2>&1
    end=$(date +%s%N)
    elapsed=$(((end - start) / 1000000))
    total=$((total + elapsed))
    echo "   Run $i: ${elapsed}ms"
done
avg=$((total / 10))
echo "   Average: ${avg}ms"
echo ""

# Memory usage (if forge is running)
echo "💾 Memory Usage:"
if pgrep -x forge > /dev/null; then
    ps -o rss= -C forge | awk '{print "   RSS:", $1/1024, "MB"}'
else
    echo "   (forge not running)"
fi
echo ""

# Test suite performance
echo "🧪 Test Suite Performance:"
time_output=$(nix develop --command bash -c "cargo test --lib --test '*' 2>&1 | grep 'test result'")
echo "   $time_output"
echo ""

# Dependency count
echo "📦 Dependencies:"
cargo tree --depth 1 | wc -l | awk '{print "   Direct dependencies:", $1-1}'
echo ""

echo "✅ Benchmark complete!"
