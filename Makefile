# forge Makefile
# Convenience wrapper for common development tasks

.PHONY: help build run test test-display check fmt clippy privacy security verify package support-bundle clean install dev watch benchmark debug

help:
	@echo "forge Development Commands"
	@echo "==========================="
	@echo ""
	@echo "Build Commands:"
	@echo "  make build      - Build release version"
	@echo "  make run        - Run in development mode"
	@echo "  make install    - Install to ~/.local/bin"
	@echo "  make package    - Build a relocatable release archive and checksum"
	@echo ""
	@echo "Quality Commands:"
	@echo "  make test       - Run all tests"
	@echo "  make test-display - Run explicit GTK/VTE regressions under Xvfb"
	@echo "  make check      - Check code without building"
	@echo "  make fmt        - Format code"
	@echo "  make clippy     - Lint code"
	@echo "  make privacy    - Scan tracked text for known personal identifiers"
	@echo "  make security   - Audit dependencies and shell scripts"
	@echo "  make verify     - Run the core build, test, syntax, and privacy gate"
	@echo ""
	@echo "Development:"
	@echo "  make dev        - Run dev script"
	@echo "  make watch      - Watch for changes and rebuild"
	@echo "  make benchmark  - Run performance benchmarks"
	@echo "  make debug      - Show debug information"
	@echo "  make support-bundle - Create a privacy-preserving support archive"
	@echo ""
	@echo "Cleanup:"
	@echo "  make clean      - Clean build artifacts"
	@echo ""

build:
	@./scripts/dev.sh build

run:
	@./scripts/dev.sh run

test:
	@./scripts/dev.sh test

test-display:
	@./scripts/dev.sh test-display

check:
	@./scripts/dev.sh check

fmt:
	@./scripts/dev.sh fmt

clippy:
	@./scripts/dev.sh clippy

privacy:
	@./scripts/privacy-check.sh

security:
	@./scripts/dev.sh security

verify:
	@./scripts/dev.sh verify

package:
	@./scripts/dev.sh package

support-bundle:
	@./scripts/support-bundle.sh

clean:
	@./scripts/dev.sh clean

install:
	@./scripts/install.sh

dev:
	@./scripts/dev.sh

watch:
	@./scripts/dev.sh watch

benchmark:
	@./scripts/benchmark.sh

debug:
	@./scripts/debug.sh info

# Default target
.DEFAULT_GOAL := help
