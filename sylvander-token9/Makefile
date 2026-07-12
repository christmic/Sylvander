.PHONY: build run dev test clean reload

# Production server — port 9527 (default config)
run: build
	token9 serve

# Development server — port 9627, isolated test DB
dev: build
	token9 --config config.test.toml serve

# Production server running from source
serve:
	cargo run -- serve

# Development server running from source
serve-dev:
	cargo run -- --config config.test.toml serve

build:
	cargo build --release --manifest-path token9-server/Cargo.toml

reload:
	curl -sS -X POST http://127.0.0.1:9527/admin/reload

reload-dev:
	curl -sS -X POST http://127.0.0.1:9627/admin/reload

test:
	cargo test

clean:
	cargo clean
