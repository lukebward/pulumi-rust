GO_TEST_FILTER_FLAG := $(if $(TEST_FILTER),-run 'TestLanguage/$(TEST_FILTER)$$',-run TestLanguage)

# The module path the version symbol lives under. It has to match
# pulumi-language-rust/go.mod exactly: the linker ignores a -X whose symbol it
# cannot resolve, silently producing an unstamped binary. Update both together
# if the repository moves.
VERSION_PKG := github.com/lukebward/pulumi-rust/pulumi-language-rust/version

# An unreleased build stamps a dev version derived from the pending changelog,
# so a locally built host reports something more useful than an empty string.
# changie is optional; without it the fallback keeps the build working, and it
# claims 0.0.0 rather than a specific release so it cannot drift into
# asserting a version that has actually shipped. The leading `v` changie emits
# is stripped so both branches produce the same shape.
FALLBACK_DEV_VERSION := 0.0.0-dev.0
DEV_VERSION := $(shell if command -v changie > /dev/null 2>&1; then changie next patch -p dev.0 | sed 's/^v//'; else echo "$(FALLBACK_DEV_VERSION)"; fi)
LD_FLAGS := -X $(VERSION_PKG).Version=$(DEV_VERSION)

# Hand-written Rust: everything except generated output. testdata/{sdks,projects}
# is what the generator emits and is snapshot-compared byte for byte, and the
# SDK vendored into the conformance-component provider carries a DO NOT EDIT
# banner for the same reason.
RUST_SOURCES := $(shell git ls-files '*.rs' | grep -v '^pulumi-language-rust/testdata/sdks/' | grep -v '^pulumi-language-rust/testdata/projects/' | grep -v '/sdks/simple/')

.PHONY: build build_sdk build_language_host test_sdk test_codegen \
	test_conformance test_all test_fast accept check_full_sdks \
	lint lint_go lint_rust format changelog clean

build: build_sdk build_language_host

build_sdk:
	cd sdk/rust/pulumi && cargo build

build_language_host:
	cd pulumi-language-rust && go build -ldflags "$(LD_FLAGS)" .

test_sdk:
	cd sdk/rust/pulumi && cargo test --locked

# The generator's own tests. No cargo, no plugins, no network.
test_codegen:
	cd pulumi-language-rust && go test ./codegen/...

test_conformance: build
	cd pulumi-language-rust && go test $(GO_TEST_FILTER_FLAG) -timeout 120m -v .

# Everything that does not need a network or a provider plugin. The pair to
# run before pushing.
test_fast: test_sdk test_codegen

# Every test in the repository. check_full_sdks is deliberately not included:
# it is a canary, not a gate. See CONTRIBUTING.md.
test_all: test_sdk test_codegen test_conformance

# Regenerate conformance snapshots (testdata/) after codegen changes.
accept: build
	cd pulumi-language-rust && PULUMI_ACCEPT=1 go test $(GO_TEST_FILTER_FLAG) -timeout 120m .

# Generate and compile the whole SDK for every provider the examples pin, then
# compile every example against it. A periodic canary, not a gate: it needs
# `pulumi` on PATH, a network and about twenty minutes, and nothing in CI
# depends on it. What it finds gets distilled into a fast test under
# pulumi-language-rust/codegen/. See CONTRIBUTING.md.
check_full_sdks:
	scripts/check-full-sdks.sh

lint: lint_go lint_rust

# golangci-lint has to run from inside the module: there is no go.mod at the
# repository root, so a ./pulumi-language-rust/... pattern from here resolves
# to nothing and the linter exits without having linted anything.
lint_go:
	cd pulumi-language-rust && go vet ./...
	cd pulumi-language-rust && golangci-lint run --config ../.golangci.yml ./...
	cd pulumi-language-rust && go mod tidy -diff

lint_rust:
	rustfmt --edition 2021 --check $(RUST_SOURCES)
	cd sdk/rust/pulumi && cargo clippy --all-targets --locked -- -D warnings

# Formats the hand-written Rust and Go.
#
# gofumpt rather than gofmt, because gofumpt is what the linter enforces and
# gofmt is a strict subset of it: formatting with gofmt would leave a tree
# `make lint` still rejects.
format:
	rustfmt --edition 2021 $(RUST_SOURCES)
	cd pulumi-language-rust && golangci-lint fmt --config ../.golangci.yml ./...

changelog:
	changie new

# The shared target directory is wherever Go's os.UserCacheDir() puts it, which
# is not XDG on macOS — it ignores XDG_CACHE_HOME and uses ~/Library/Caches, so
# a single XDG-only path would silently leave gigabytes behind there.
clean:
	cd sdk/rust/pulumi && cargo clean
	rm -f pulumi-language-rust/pulumi-language-rust
	if [ "$$(uname)" = Darwin ]; then \
		rm -rf "$$HOME/Library/Caches/pulumi-language-rust/target"; \
	else \
		rm -rf "$${XDG_CACHE_HOME:-$$HOME/.cache}/pulumi-language-rust/target"; \
	fi
