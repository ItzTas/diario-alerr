.PHONY: check lint test generate dev up down

check: lint test

lint:
	test -z "$$(gofmt -l .)"
	go vet ./...
	golangci-lint run ./...

test:
	go test -race ./...

generate:
	go tool sqlc generate

dev:
	go tool air

up:
	docker compose up -d --build

down:
	docker compose down
