.PHONY: help build-arm build-x86 build-mac build-all release-archives github-release \
       build-agent-arm build-agent-x86 build-agent-mac build-agent-all \
       build-all-agents build-all-agents-arm build-all-agents-x86 build-all-agents-mac

.DEFAULT_GOAL := help

# All agents in the workspace (package-name:binary-name)
AGENTS := orderbook-cloud-agent:cloud-agent agent-spot-dca:agent-spot-dca agent-tpsl:agent-tpsl

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-30s %s\n", $$1, $$2}'

# ---------------------------------------------------------------------------
# Single agent builds (usage: make build-agent-arm AGENT=agent-spot-dca)
# ---------------------------------------------------------------------------

build-agent-arm: ## Build one agent for Linux ARM64 (usage: make build-agent-arm AGENT=agent-spot-dca)
	@if [ -z "$(AGENT)" ]; then echo "Error: AGENT is required (e.g. AGENT=agent-spot-dca)"; exit 1; fi
	@PACKAGE=$(AGENT); BIN=$(AGENT); \
	for entry in $(AGENTS); do \
		p=$$(echo $$entry | cut -d: -f1); b=$$(echo $$entry | cut -d: -f2); \
		if [ "$$p" = "$(AGENT)" ] || [ "$$b" = "$(AGENT)" ]; then PACKAGE=$$p; BIN=$$b; break; fi; \
	done; \
	echo "Building $$BIN for Linux ARM64..."; \
	mkdir -p docker/release/$$BIN/arm; \
	DOCKER_BUILDKIT=1 docker build \
		--platform linux/arm64 \
		--build-arg AGENT_PACKAGE=$$PACKAGE \
		--build-arg AGENT_BIN=$$BIN \
		-f docker/Dockerfile \
		-t $$BIN-builder:arm64 \
		--progress=plain \
		.; \
	docker create --name $$BIN-extract $$BIN-builder:arm64; \
	docker cp $$BIN-extract:/output/$$BIN docker/release/$$BIN/arm/$$BIN; \
	docker rm $$BIN-extract; \
	docker rmi $$BIN-builder:arm64 2>/dev/null || true; \
	echo "$$BIN built for ARM64 → docker/release/$$BIN/arm/$$BIN"

build-agent-x86: ## Build one agent for Linux x86_64 (usage: make build-agent-x86 AGENT=agent-spot-dca)
	@if [ -z "$(AGENT)" ]; then echo "Error: AGENT is required (e.g. AGENT=agent-spot-dca)"; exit 1; fi
	@PACKAGE=$(AGENT); BIN=$(AGENT); \
	for entry in $(AGENTS); do \
		p=$$(echo $$entry | cut -d: -f1); b=$$(echo $$entry | cut -d: -f2); \
		if [ "$$p" = "$(AGENT)" ] || [ "$$b" = "$(AGENT)" ]; then PACKAGE=$$p; BIN=$$b; break; fi; \
	done; \
	echo "Building $$BIN for Linux x86_64..."; \
	mkdir -p docker/release/$$BIN/x86; \
	DOCKER_BUILDKIT=1 docker build \
		--platform linux/amd64 \
		--build-arg AGENT_PACKAGE=$$PACKAGE \
		--build-arg AGENT_BIN=$$BIN \
		-f docker/Dockerfile \
		-t $$BIN-builder:amd64 \
		--progress=plain \
		.; \
	docker create --name $$BIN-extract $$BIN-builder:amd64; \
	docker cp $$BIN-extract:/output/$$BIN docker/release/$$BIN/x86/$$BIN; \
	docker rm $$BIN-extract; \
	docker rmi $$BIN-builder:amd64 2>/dev/null || true; \
	echo "$$BIN built for x86_64 → docker/release/$$BIN/x86/$$BIN"

build-agent-mac: ## Build one agent for macOS Silicon (usage: make build-agent-mac AGENT=agent-spot-dca)
	@if [ -z "$(AGENT)" ]; then echo "Error: AGENT is required (e.g. AGENT=agent-spot-dca)"; exit 1; fi
	@PACKAGE=$(AGENT); BIN=$(AGENT); \
	for entry in $(AGENTS); do \
		p=$$(echo $$entry | cut -d: -f1); b=$$(echo $$entry | cut -d: -f2); \
		if [ "$$p" = "$(AGENT)" ] || [ "$$b" = "$(AGENT)" ]; then PACKAGE=$$p; BIN=$$b; break; fi; \
	done; \
	echo "Building $$BIN for macOS Silicon..."; \
	mkdir -p docker/release/$$BIN/mac; \
	cargo build --release -p $$PACKAGE; \
	cp target/release/$$BIN docker/release/$$BIN/mac/$$BIN; \
	echo "$$BIN built for macOS → docker/release/$$BIN/mac/$$BIN"

build-agent-all: ## Build one agent for all platforms (usage: make build-agent-all AGENT=agent-spot-dca)
	@$(MAKE) build-agent-arm AGENT=$(AGENT)
	@$(MAKE) build-agent-x86 AGENT=$(AGENT)
	@$(MAKE) build-agent-mac AGENT=$(AGENT)

# ---------------------------------------------------------------------------
# All agents at once
# ---------------------------------------------------------------------------

build-all-agents-arm: ## Build all agents for Linux ARM64
	@for entry in $(AGENTS); do \
		p=$$(echo $$entry | cut -d: -f1); \
		$(MAKE) build-agent-arm AGENT=$$p; \
	done

build-all-agents-x86: ## Build all agents for Linux x86_64
	@for entry in $(AGENTS); do \
		p=$$(echo $$entry | cut -d: -f1); \
		$(MAKE) build-agent-x86 AGENT=$$p; \
	done

build-all-agents-mac: ## Build all agents for macOS Silicon
	@for entry in $(AGENTS); do \
		p=$$(echo $$entry | cut -d: -f1); \
		$(MAKE) build-agent-mac AGENT=$$p; \
	done

build-all-agents: ## Build all agents for all platforms
	@for entry in $(AGENTS); do \
		p=$$(echo $$entry | cut -d: -f1); \
		$(MAKE) build-agent-all AGENT=$$p; \
	done

# ---------------------------------------------------------------------------
# Legacy cloud-agent shortcuts (backwards compatible)
# ---------------------------------------------------------------------------

build-arm: ## Build cloud-agent for Linux ARM64
	@$(MAKE) build-agent-arm AGENT=orderbook-cloud-agent

build-x86: ## Build cloud-agent for Linux x86_64
	@$(MAKE) build-agent-x86 AGENT=orderbook-cloud-agent

build-mac: ## Build cloud-agent for macOS Silicon
	@$(MAKE) build-agent-mac AGENT=orderbook-cloud-agent

build-all: build-arm build-x86 build-mac ## Build cloud-agent for all platforms

# ---------------------------------------------------------------------------
# Release
# ---------------------------------------------------------------------------

release-archives: build-all-agents ## Build all agents and create release archives
	@echo "Creating release archives..."
	@mkdir -p docker/release/github
	@for entry in $(AGENTS); do \
		BIN=$$(echo $$entry | cut -d: -f2); \
		echo "Archiving $$BIN..."; \
		cd docker/release/$$BIN/arm && tar -czf ../../github/$$BIN-arm64-linux.tar.gz $$BIN && cd ../../../..; \
		cd docker/release/$$BIN/x86 && tar -czf ../../github/$$BIN-x86_64-linux.tar.gz $$BIN && cd ../../../..; \
		cd docker/release/$$BIN/mac && tar -czf ../../github/$$BIN-macos-silicon.tar.gz $$BIN && cd ../../../..; \
	done
	@echo "Calculating checksums..."
	@cd docker/release/github && shasum -a 256 *.tar.gz > checksums.txt
	@echo "Release archives created:"
	@ls -lh docker/release/github/

# Usage: make github-release VERSION=v1.0.0
github-release: release-archives ## Create a GitHub release (usage: make github-release VERSION=v1.0.0)
	@if [ -z "$(VERSION)" ]; then \
		echo "Error: VERSION is required"; \
		echo "Usage: make github-release VERSION=v1.0.0"; \
		exit 1; \
	fi
	@echo "Creating GitHub release $(VERSION)"
	@if ! command -v gh >/dev/null 2>&1; then \
		echo "Error: GitHub CLI (gh) is not installed"; \
		echo "Install from: https://cli.github.com/"; \
		exit 1; \
	fi
	@echo "Fetching latest main branch..."
	@git fetch origin main
	@echo "Creating git tag $(VERSION) from origin/main..."
	@git tag -a $(VERSION) -m "Release $(VERSION)" origin/main 2>/dev/null || echo "Tag already exists"
	@git push origin $(VERSION) 2>/dev/null || echo "Tag already pushed"
	@echo "Creating GitHub release..."
	@gh release create $(VERSION) \
		--title "Silvana Agents $(VERSION)" \
		--generate-notes \
		docker/release/github/*.tar.gz \
		docker/release/github/checksums.txt
	@echo "Release $(VERSION) created successfully!"
