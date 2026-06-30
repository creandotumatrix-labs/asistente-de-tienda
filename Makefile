.PHONY: help build run demo test verify fmt lint clean

help:
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'

build: ## Compile (debug)
	cargo build

run: ## Interactive WhatsApp simulator (needs ANTHROPIC_API_KEY)
	cargo run --quiet --bin tienda -- --debug

demo: ## Scripted wow-flow: out-of-catalog → real product → order status
	@cargo run --quiet --bin tienda -- --no-banner --debug --once "tienen la bici voladora 3000?"
	@cargo run --quiet --bin tienda -- --no-banner --debug --once "tienen la mochila Vortex en negro? hacen envío a Guadalajara?"
	@cargo run --quiet --bin tienda -- --no-banner --debug --once "dónde va mi pedido #10482?"

test: ## Run the guardrail suite (no network)
	cargo test

verify: ## Executable logic oracle over the seed data (no Rust toolchain needed)
	python3 scripts/verify_logic.py

fmt: ## Format
	cargo fmt

lint: ## Clippy (warnings as errors)
	cargo clippy --all-targets -- -D warnings

clean:
	cargo clean
