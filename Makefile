GO_TEST_FILTER_FLAG := $(if $(TEST_FILTER),-run 'TestLanguage/$(TEST_FILTER)$$',-run TestLanguage)

.PHONY: build build_sdk build_language_host test_sdk test_codegen \
	test_conformance accept check_full_sdks

build: build_sdk build_language_host

build_sdk:
	cd sdk/rust/pulumi && cargo build

build_language_host:
	cd pulumi-language-rust && go build .

test_sdk:
	cd sdk/rust/pulumi && cargo test

# The generator's own tests. No cargo, no plugins, no network.
test_codegen:
	cd pulumi-language-rust && go test ./codegen/...

test_conformance: build
	cd pulumi-language-rust && go test $(GO_TEST_FILTER_FLAG) -timeout 120m -v .

# Regenerate conformance snapshots (testdata/) after codegen changes.
accept: build
	cd pulumi-language-rust && PULUMI_ACCEPT=1 go test $(GO_TEST_FILTER_FLAG) -timeout 120m .

# Generate and compile the whole SDK for every provider the examples pin.
# Needs `pulumi` on PATH and a network; not run by CI. Slow — tens of
# megabytes of Rust per provider.
check_full_sdks:
	scripts/check-full-sdks.sh
