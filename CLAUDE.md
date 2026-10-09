# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Reliost is a prototype of an Eliot symbol server rewrite in Rust. It's a web service that provides symbolication services for crash reports and stack traces.

## Repository Type

This is a Jujutsu (jj) repository. Use `jj` commands instead of `git` commands for version control operations (e.g., `jj commit` instead of `git commit`).

## Architecture

### Core Components

- **Symbol Manager**: Handles symbol resolution using the `wholesym` library, which supports both Breakpad (.sym) and Windows (.pdb) symbols
- **Quota Manager**: Manages automatic file deletion in the cache directory to enforce size and age limits
- **Web Server**: Built with Actix-web, provides REST API endpoints for symbolication

### Configuration System

The application uses a layered configuration approach:
- Base configuration: `configuration/base.toml`
- Environment-specific: `configuration/local.toml` or `configuration/production.toml`
- Environment variables: `RELIOST_*` prefix (e.g., `RELIOST_SERVER_PORT=5001`)

Configuration priority: Environment variables > Environment-specific config > Base config

### API Endpoints

- `/symbolicate/v5` - Main symbolication endpoint (POST)
- `/asm/v1` - Assembly code retrieval endpoint (POST)
- `/deobfuscate/java/v1` - Java stack frame deobfuscation using Android R8/ProGuard mapping files (POST). The request and response format is documented in `src/proguard/api.rs`.
- `/__heartbeat__`, `/__lbheartbeat__`, `/__version__` - Dockerflow health checks

## Development Commands

### Build and Run
```bash
cargo build              # Build the project
cargo build --release    # Build optimized release version
cargo run                # Run the server (defaults to localhost:8080)
```

### Testing
```bash
cargo test               # Run all tests
cargo test --lib         # Run unit tests only
cargo test --test integration_tests  # Run integration tests
```

### Code Quality
```bash
cargo fmt                # Format code
cargo fmt -- --check     # Check formatting without making changes
cargo clippy             # Run linter
cargo clippy -- -Dwarnings  # Run linter treating warnings as errors (CI requirement)
```

### Environment Variables
```bash
APP_ENVIRONMENT=local    # Set environment (local/production)
RELIOST_SERVER_HOST=0.0.0.0
RELIOST_SERVER_PORT=8080
```

## Symbol Server Configuration

The server can fetch symbols from multiple sources configured in `configuration/base.toml`:
- Breakpad symbols from configured servers (e.g., Mozilla's symbol server)
- Windows symbols from Microsoft's symbol server
- Local symbol directories
- Android R8/ProGuard mapping files from the `[proguard]` servers, from `<server>/<proguard uuid>/mapping.txt.zst` (zstd-compressed, lowercase dashed UUID). They're decompressed and converted into `ProguardCache` files on download. `scripts/extract-aab-mapping.py` creates these files from an `.aab` file.

Symbols are cached locally in `./cache/symbols/` with automatic cleanup managed by the quota system.