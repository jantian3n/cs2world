TAGS=with_gvisor,with_quic,with_utls,with_clash_api
VERSION=0.0.1
LDFLAGS=-s -w -X 'github.com/sagernet/sing-box/constant.Version=1.14.4-cs2world-$(VERSION)'

.PHONY: build-cs2world build-cs2world-linux build-cs2world-win build-all test-cs2world test clean

build-cs2world:
	go build -tags "$(TAGS)" -trimpath -ldflags="$(LDFLAGS)" -o bin/sing-box-cs2world ./cmd/sing-box-cs2world

build-cs2world-linux:
	CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -tags "$(TAGS)" -trimpath -ldflags="$(LDFLAGS)" -o bin/sing-box-cs2world-linux-amd64 ./cmd/sing-box-cs2world

build-cs2world-linux-arm64:
	CGO_ENABLED=0 GOOS=linux GOARCH=arm64 go build -tags "$(TAGS)" -trimpath -ldflags="$(LDFLAGS)" -o bin/sing-box-cs2world-linux-arm64 ./cmd/sing-box-cs2world

build-cs2world-win:
	CGO_ENABLED=0 GOOS=windows GOARCH=amd64 go build -tags "$(TAGS)" -trimpath -ldflags="$(LDFLAGS)" -o bin/sing-box-cs2world-windows-amd64.exe ./cmd/sing-box-cs2world

build-all: build-cs2world build-cs2world-linux build-cs2world-win

test-cs2world: build-cs2world
	python3 scripts/test_singbox_cs2world.py

build-standalone:
	go build -trimpath -ldflags="$(LDFLAGS)" -o bin/cs2-ghost ./cmd/cs2-ghost

test: test-cs2world

clean:
	rm -rf bin/
