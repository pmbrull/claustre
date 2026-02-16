.PHONY: build test lint fmt check cov cov-html cov-store clean

# Build
build:
	cargo build

release:
	cargo build --release

# Test
test:
	cargo test

# Lint & format
lint:
	cargo clippy

fmt:
	cargo fmt --check

# All checks (CI-equivalent)
check: fmt lint test

# Coverage — requires: cargo install cargo-llvm-cov && rustup component add llvm-tools-preview
cov:
	cargo llvm-cov --text --summary-only

cov-report:
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

cov-html:
	cargo llvm-cov --html --output-dir coverage
	@echo "Coverage report: coverage/html/index.html"

cov-store:
	cargo llvm-cov --text -- store 2>/dev/null | head -5

# Clean
clean:
	cargo clean
	rm -rf coverage/
