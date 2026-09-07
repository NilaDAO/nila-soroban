.PHONY: test build fmt clippy check wasm deploy-testnet

test:
	cargo test --workspace --all-features

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

check: fmt clippy test

# Production WASM (stellar-cli applies the Soroban-required flags).
# Fallback: cargo with the workspace release profile + wasm32v1-none.
build:
	@if command -v stellar >/dev/null 2>&1; then \
		stellar contract build; \
	else \
		echo "stellar CLI not found; building with cargo (install: cargo install --locked stellar-cli)"; \
		cargo build --release --target wasm32v1-none; \
	fi

wasm: build
	@ls -lh target/wasm32v1-none/release/*.wasm 2>/dev/null || true

deploy-testnet:
	bash scripts/deploy-testnet.sh
