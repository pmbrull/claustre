.DEFAULT_GOAL := help

MIN_COVERAGE := 60

.PHONY: help
help: ## Show this help message
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*## "}; {printf "\033[35m%-35s\033[0m %s\n", $$1, $$2}'

## -------
## Build
## -------

.PHONY: build
build: ## Build in debug mode
	cargo build

.PHONY: release
release: ## Build in release mode
	cargo build --release

.PHONY: install
install: ## Install claustre binary via cargo install
	cargo install --path .

## -------
## Test
## -------

.PHONY: test
test: ## Run all tests
	cargo test

## -------
## Lint & Format
## -------

.PHONY: lint
lint: ## Run clippy linter
	cargo clippy

.PHONY: fmt
fmt: ## Check code formatting
	cargo fmt --check

## -------
## Checks
## -------

.PHONY: check
check: fmt lint test cov-gate ## Run all checks (CI-equivalent)

## -------
## Coverage (requires: cargo install cargo-llvm-cov && rustup component add llvm-tools-preview)
## -------

.PHONY: cov-gate
cov-gate: ## Fail if coverage is below $(MIN_COVERAGE)%
	@cargo llvm-cov --json 2>/dev/null | python3 -c "\
	import json, sys; \
	data = json.load(sys.stdin); \
	pct = data['data'][0]['totals']['lines']['percent']; \
	print(f'Coverage: {pct:.1f}%'); \
	sys.exit(1) if pct < $(MIN_COVERAGE) else None" \
	|| (echo "FAIL: coverage below $(MIN_COVERAGE)%" && exit 1)

.PHONY: cov
cov: ## Show coverage summary
	cargo llvm-cov --text --summary-only

.PHONY: cov-report
cov-report: ## Show per-file coverage report
	cargo llvm-cov --json | python3 -c "\
	import json, sys; \
	data = json.load(sys.stdin); \
	t = data['data'][0]['totals']; \
	print(f\"Lines:     {t['lines']['percent']:.1f}% ({t['lines']['covered']}/{t['lines']['count']})\"); \
	print(f\"Functions: {t['functions']['percent']:.1f}% ({t['functions']['covered']}/{t['functions']['count']})\"); \
	print(); \
	print(f\"{'File':<55} {'Lines':>8} {'Covered':>8} {'Pct':>7}\"); \
	print('-' * 82); \
	[print(f\"{f['filename'].split('claustre/')[-1]:<55} {f['summary']['lines']['count']:>8} {f['summary']['lines']['covered']:>8} {f['summary']['lines']['percent']:>6.1f}%\") \
	 for f in sorted(data['data'][0]['files'], key=lambda x: x['filename'])]"

.PHONY: cov-html
cov-html: ## Generate HTML coverage report
	cargo llvm-cov --html --output-dir coverage
	@echo "Coverage report: coverage/html/index.html"

.PHONY: cov-store
cov-store: ## Show coverage for store module only
	cargo llvm-cov --text -- store 2>/dev/null | head -5

## -------
## Docs
## -------

.PHONY: docs
docs: ## Run docs site locally (http://localhost:4321)
	cd docs && npm run dev

.PHONY: docs-build
docs-build: ## Build docs site to docs/dist/
	cd docs && npm run build

## -------
## Clean
## -------

.PHONY: clean
clean: ## Remove build artifacts and coverage reports
	cargo clean
	rm -rf coverage/
