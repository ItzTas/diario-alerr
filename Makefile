.PHONY: check test up down

check:
	cargo fmt --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test

test:
	cargo test

up:
	docker compose up -d

down:
	docker compose down
