.PHONY: check check-full fmt build bench app

check:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace

# Fast set plus #[ignore = "slow: ..."] tests, in release. Run before finishing an analysis/engine task.
check-full:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace --release -- --include-ignored

fmt:
	cargo fmt --all

build:
	cargo build --release --workspace

# Criterion benches (add benches per crate as they appear).
bench:
	cargo bench --workspace

# WingMix.app (SwiftUI, apps/WingMix): daemon release build, app bundle, Swift tests (incl. the real daemon over
# its socket) and a launch/quit smoke test. Not part of `make check`.
app:
	cargo build --release -p wing-mixd
	apps/WingMix/build.sh
	cd apps/WingMix && swift test
	apps/WingMix/smoke.sh
