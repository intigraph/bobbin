# Bobbin: Semantic code indexing
# Run `just --list` to see available recipes

# Quiet by default to save context; use verbose=true for full output
verbose := "false"
cargo_flags := if verbose == "true" { "" } else { "-q --message-format=short" }

# `knowledge` gates bobbin's ENTIRE Quipu integration: the MCP surface, the coupling
# exporter, and PPR context reranking. It is NOT a cargo default feature, so a plain
# `cargo build` compiles all of it out SILENTLY — which is how it shipped dark for
# months (bobbin-jdlkh). Build and test it everywhere, or its tests guard nothing.
features := "knowledge"
cargo_features := "--features " + features

# Default recipe - show available commands
default:
    @just --list

# === Development ===

# Build the project (quiet by default, use verbose=true for full output)
build:
    cargo build {{cargo_flags}} {{cargo_features}}

# Run tests (quiet by default, use verbose=true for full output)
test:
    cargo test {{cargo_flags}} {{cargo_features}}

# The eval framework's Python tests. Deliberately NOT folded into `just test`:
# they need pytest and matplotlib, which a Rust contributor has no reason to
# have, and a `just test` that fails on a missing python package trains people
# to stop running it. CI runs this on every push (job `eval-tests`), so it is
# gated where it counts — this recipe is for running it locally, not the gate.
test-eval:
    cd eval && python3 -m pytest tests/ scorer/test_scorer.py -q

# Type check and pre-commit hooks (quiet by default, use verbose=true for full output)
check:
    cargo check {{cargo_flags}} {{cargo_features}}
    pre-commit run --all-files
    bash scripts/check-file-size.sh --all

# Lint with clippy (quiet by default, use verbose=true for full output)
lint:
    cargo clippy {{cargo_flags}} {{cargo_features}}

# Build and run
run *args:
    cargo run {{cargo_flags}} {{cargo_features}} -- {{args}}

# === Setup ===

# Install system dependencies (idempotent, Linux apt / macOS brew)
setup:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Checking system dependencies..."
    # Rust toolchain
    if command -v rustc &>/dev/null; then
        echo "  rustc $(rustc --version | awk '{print $2}') ✓"
    else
        echo "  rustc not found — install via https://rustup.rs"
        exit 1
    fi
    # C++ compiler (needed by cc-rs crate)
    if command -v c++ &>/dev/null; then
        echo "  c++ ✓"
    else
        echo "  c++ not found — installing..."
        if command -v apt-get &>/dev/null; then
            sudo apt-get install -y -qq g++
        elif command -v brew &>/dev/null; then
            echo "  Xcode CLT provides c++ on macOS. Run: xcode-select --install"
            exit 1
        else
            echo "  Please install a C++ compiler manually."
            exit 1
        fi
    fi
    # protoc (needed by lancedb transitive dep lance-encoding)
    if command -v protoc &>/dev/null; then
        echo "  protoc $(protoc --version | awk '{print $2}') ✓"
    else
        echo "  protoc not found — installing..."
        if command -v apt-get &>/dev/null; then
            sudo apt-get install -y -qq protobuf-compiler
        elif command -v brew &>/dev/null; then
            brew install protobuf
        else
            echo "  Please install protobuf-compiler manually."
            exit 1
        fi
    fi
    # tokei (LOC counter, needed by eval framework)
    if command -v tokei &>/dev/null; then
        echo "  tokei $(tokei --version | awk '{print $2}') ✓"
    else
        echo "  tokei not found — installing..."
        cargo install tokei
    fi
    # uv (Python package manager, needed by eval framework)
    if command -v uv &>/dev/null; then
        echo "  uv $(uv --version | awk '{print $2}') ✓"
    else
        echo "  uv not found — installing..."
        curl -LsSf https://astral.sh/uv/install.sh | sh
    fi
    echo "All dependencies satisfied."
    # Install pre-commit hooks
    if command -v pre-commit &>/dev/null; then
        pre-commit install
        echo "  pre-commit hooks installed ✓"
    else
        echo "  pre-commit not found — install via: pip install pre-commit"
    fi

# === Documentation ===

# Documentation management: just docs <cmd>
# Commands: build, serve, lint, fix, fmt, vale, validate, coverage, check
docs cmd="build":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{cmd}}" in
        build)    mdbook build docs/book ;;
        serve)    mdbook serve docs/book --open ;;
        lint)     npx markdownlint-cli2 "docs/book/src/**/*.md" "README.md" "CONTRIBUTING.md" ;;
        fix)      npx markdownlint-cli2 --fix "docs/book/src/**/*.md" "README.md" "CONTRIBUTING.md" ;;
        fmt)      npx prettier --write "docs/book/src/**/*.md" --prose-wrap preserve ;;
        vale)     vale docs/book/src/ ;;
        validate) bash scripts/test-validate-frontmatter.sh && bash scripts/validate-frontmatter.sh ;;
        coverage) bash scripts/doc-coverage.sh ;;
        check)    just docs lint && just docs vale && just docs validate && just docs build ;;
        *)        echo "Unknown: {{cmd}}. Try: build serve lint fix fmt vale validate coverage check" ;;
    esac

# === Eval Framework ===

# Default eval settings
eval_model := "claude-opus-4-6"
eval_attempts := "1"
eval_approaches := "both"
eval_budget := "100.00"
eval_timeout := "3600"
eval_index_timeout := "600"

# Runtime paths are discovered by Bobbin or supplied by the caller. Do not
# override them globally: a host-specific path also breaks CPU-only tests.

# Run a single eval task: just eval-task flask-001
eval-task task_id:
    cd eval && python3 -m runner.cli run-task {{task_id}} --attempts {{eval_attempts}} --approaches {{eval_approaches}} --model {{eval_model}} --budget {{eval_budget}} --timeout {{eval_timeout}} --index-timeout {{eval_index_timeout}} --skip-verify

# Run all eval tasks sequentially
eval-all:
    cd eval && python3 -m runner.cli run-all --attempts {{eval_attempts}} --approaches {{eval_approaches}} --model {{eval_model}} --budget {{eval_budget}} --timeout {{eval_timeout}} --index-timeout {{eval_index_timeout}} --skip-verify

# Score existing eval results
eval-score:
    cd eval && python3 -m runner.cli score results

# Generate eval report
eval-report:
    cd eval && python3 -m runner.cli report results

# Publish eval results to mdbook
eval-publish run_id="":
    #!/usr/bin/env bash
    set -euo pipefail
    cd eval
    if [ -n "{{run_id}}" ]; then
        python3 -m runner.cli publish --run "{{run_id}}" --all-runs
    else
        python3 -m runner.cli publish --all-runs
    fi

# Run LLM judge on eval results
eval-judge run_id="":
    #!/usr/bin/env bash
    set -euo pipefail
    cd eval
    if [ -n "{{run_id}}" ]; then
        python3 -m runner.cli judge results --run "{{run_id}}"
    else
        python3 -m runner.cli judge results
    fi

# Setup eval prerequisites
eval-setup:
    cd eval && bash setup-eval.sh

# Exercise the real changelog renderer on synthetic release commits.
test-changelog:
    python3 scripts/test_changelog_scrub.py

# Verify the standalone installer using private release fixtures.
installer-check:
    sh -n install.sh
    python3 scripts/test-install-release.py

# Focused stack prime checks; runtime tests require the crew HOME guard.
prime action="test":
    #!/usr/bin/env bash
    set -euo pipefail
    case {{quote(action)}} in
        check) cargo check --locked --features knowledge --all-targets ;;
        check-default) cargo check --locked --all-targets ;;
        lint) cargo clippy --locked --features knowledge --all-targets ;;
        fmt) cargo fmt --check ;;
        format) cargo fmt ;;
        size) bash scripts/check-file-size.sh --all ;;
        ratchet) bash scripts/check-file-size.sh --update-allowlist ;;
        test) cargo test --locked --features knowledge --bin bobbin cli::prime:: && cargo test --locked --features knowledge --test cli_prime --test prime_sync ;;
        *) echo 'Use: just prime check|check-default|lint|fmt|format|size|ratchet|test' >&2; exit 2 ;;
    esac
