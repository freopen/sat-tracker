.DEFAULT_GOAL := check

.PHONY: check rustfmt clippy clippy-all-features tests tests-e2e workflows diff-check

check: rustfmt clippy clippy-all-features tests tests-e2e workflows diff-check

rustfmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets --locked -- -D warnings

clippy-all-features:
	cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

tests:
	cargo test --workspace --locked

tests-e2e:
	cargo test --workspace --locked --features e2e -- --test-threads=1

workflows:
	actionlint -shellcheck= -pyflakes=

diff-check:
	git --no-pager diff --no-ext-diff --no-textconv --check HEAD
