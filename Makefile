.PHONY: help build-arm build-x86 build-mac build-all release-archives github-release

.DEFAULT_GOAL := help

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-20s %s\n", $$1, $$2}'

build-arm: ## Build cloud-agent for Ubuntu Linux ARM64 (aarch64) using Docker
	@echo "Building cloud-agent for Ubuntu Linux ARM64..."
	@mkdir -p docker/release/arm
	@DOCKER_BUILDKIT=1 docker build \
		--platform linux/arm64 \
		-f docker/Dockerfile \
		-t cloud-agent-builder:arm64 \
		--progress=plain \
		.
	@echo "Extracting binary from Docker image..."
	@docker create --name cloud-agent-extract cloud-agent-builder:arm64
	@docker cp cloud-agent-extract:/output/cloud-agent docker/release/arm/cloud-agent
	@docker rm cloud-agent-extract
	@docker rmi cloud-agent-builder:arm64 2>/dev/null || true
	@echo "cloud-agent built successfully for ARM64"
	@echo "Binary location: docker/release/arm/cloud-agent"

build-x86: ## Build cloud-agent for Ubuntu Linux x86_64 (amd64) using Docker
	@echo "Building cloud-agent for Ubuntu Linux x86_64..."
	@mkdir -p docker/release/x86
	@DOCKER_BUILDKIT=1 docker build \
		--platform linux/amd64 \
		-f docker/Dockerfile \
		-t cloud-agent-builder:amd64 \
		--progress=plain \
		.
	@echo "Extracting binary from Docker image..."
	@docker create --name cloud-agent-extract cloud-agent-builder:amd64
	@docker cp cloud-agent-extract:/output/cloud-agent docker/release/x86/cloud-agent
	@docker rm cloud-agent-extract
	@docker rmi cloud-agent-builder:amd64 2>/dev/null || true
	@echo "cloud-agent built successfully for x86_64"
	@echo "Binary location: docker/release/x86/cloud-agent"

build-mac: ## Build cloud-agent for macOS Apple Silicon (M1/M2/M3) natively
	@echo "Building cloud-agent for macOS Apple Silicon..."
	@mkdir -p docker/release/mac
	@cargo build --release --bin cloud-agent
	@cp target/release/cloud-agent docker/release/mac/cloud-agent
	@echo "cloud-agent built successfully for macOS Apple Silicon"
	@echo "Binary location: docker/release/mac/cloud-agent"

build-all: build-arm build-x86 build-mac ## Build cloud-agent for all platforms (Linux ARM64, x86_64, macOS Silicon)
	@echo "Built cloud-agent for all platforms"

release-archives: build-all ## Build and create release archives for all platforms
	@echo "Creating release archives..."
	@mkdir -p docker/release/github
	@echo "Creating ARM64 Linux archive..."
	@cd docker/release/arm && tar -czf ../github/cloud-agent-arm64-linux.tar.gz cloud-agent
	@echo "Creating x86_64 Linux archive..."
	@cd docker/release/x86 && tar -czf ../github/cloud-agent-x86_64-linux.tar.gz cloud-agent
	@echo "Creating macOS Apple Silicon archive..."
	@cd docker/release/mac && tar -czf ../github/cloud-agent-macos-silicon.tar.gz cloud-agent
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
		--title "Cloud Agent $(VERSION)" \
		--generate-notes \
		docker/release/github/cloud-agent-arm64-linux.tar.gz \
		docker/release/github/cloud-agent-x86_64-linux.tar.gz \
		docker/release/github/cloud-agent-macos-silicon.tar.gz \
		docker/release/github/checksums.txt
	@echo "Release $(VERSION) created successfully!"
