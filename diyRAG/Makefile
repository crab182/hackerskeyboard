# diyRAG — Makefile (mirror of justfile for environments without `just`).
# See MASTER_BUILD_SPEC.md §17 (deployment) and §20 (phases). Targets wrap
# docker compose / cargo / the diyrag CLI.

# Load .env if present so DATABASE_URL etc. are available to recipes.
ifneq (,$(wildcard ./.env))
include .env
export
endif

COMPOSE      := docker compose -f docker-compose.yml
COMPOSE_GPU  := docker compose -f docker-compose.yml -f docker-compose.gpu.yml
COMPOSE_DEV  := docker compose -f docker-compose.yml -f docker-compose.dev.yml

# Optional arg passthrough, e.g. `make logs SERVICE=core-api`
SERVICE ?=
DIR     ?= ./snapshots
ARGS    ?=

.DEFAULT_GOAL := help
.PHONY: help up up-gpu down logs ps dev test lint audit migrate seed bootstrap \
        bootstrap-env snapshot restore svc-install svc-status

help: ## List available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
	  | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

## --- Config / secrets -------------------------------------------------------

bootstrap-env: ## Generate a local .env with strong secrets (gitignored; pass ARGS=--force)
	bash scripts/gen-env.sh $(ARGS)

## --- Stack lifecycle --------------------------------------------------------

up: ## Bring up the base stack (Linux/unraid primary path)
	$(COMPOSE) up -d
	@echo "Stack up. Gateway: https://localhost:8443  |  logs: make logs"

up-gpu: ## Bring up the GPU overlay (NVIDIA Container Toolkit)
	$(COMPOSE_GPU) up -d

down: ## Tear the stack down (keeps named volumes)
	$(COMPOSE) down

logs: ## Tail logs for a service: make logs SERVICE=core-api
	$(COMPOSE) logs -f $(SERVICE)

ps: ## Show stack status
	$(COMPOSE) ps

## --- Development ------------------------------------------------------------

dev: ## Hot-reload dev stack (cargo-watch + exposed debug ports)
	$(COMPOSE_DEV) up

## --- Quality gates (§18, §19) ----------------------------------------------

test: ## Run Rust workspace + Python sidecar tests
	cargo test --workspace --all-features
	cd services-py/gpu-runtime && python -m pytest -q
	cd services-py/parsing-service && python -m pytest -q

lint: ## rustfmt + clippy (deny warnings) + ruff
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	ruff check services-py

audit: ## Supply-chain checks: cargo-deny + cargo-audit
	cargo deny check
	cargo audit

## --- Database / data --------------------------------------------------------

migrate: ## Apply SQL migrations via sqlx
	sqlx migrate run --source migrations

seed: ## Seed admin tenant/user + help-content store
	cargo run -p diyrag-cli -- seed --admin --help-content

bootstrap: ## First-run: CA + certs (rcgen), models, migrations, seed
	cargo run -p diyrag-cli -- bootstrap --generate-ca --pull-models
	$(MAKE) migrate
	$(MAKE) seed

## --- Backup / replication (§9, §17) ----------------------------------------

snapshot: ## Qdrant snapshot + Postgres dump (unit of vector replication)
	cargo run -p diyrag-cli -- node snapshot --out ./snapshots

restore: ## Restore from a snapshot directory: make restore DIR=./snapshots
	cargo run -p diyrag-cli -- node restore --from $(DIR)

## --- Service management (§16b) ---------------------------------------------

svc-install: ## Install diyRAG as an OS service (SCM auto-start / systemd / compose)
	cargo run -p diyrag-cli -- service install --mode all-in-one --auto-start

svc-status: ## Query the OS service status
	cargo run -p diyrag-cli -- service status
