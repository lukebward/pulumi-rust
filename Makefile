GO_TEST_FILTER_FLAG := $(if $(TEST_FILTER),-run 'TestLanguage/$(TEST_FILTER)$$',-run TestLanguage)

.PHONY: build build_sdk build_language_host test_sdk test_conformance accept

build: build_sdk build_language_host

build_sdk:
	cd sdk/rust/pulumi && cargo build

build_language_host:
	cd pulumi-language-rust && go build .

test_sdk:
	cd sdk/rust/pulumi && cargo test

test_conformance: build
	cd pulumi-language-rust && go test $(GO_TEST_FILTER_FLAG) -timeout 120m -v .

# Regenerate conformance snapshots (testdata/) after codegen changes.
accept: build
	cd pulumi-language-rust && PULUMI_ACCEPT=1 go test $(GO_TEST_FILTER_FLAG) -timeout 120m .
