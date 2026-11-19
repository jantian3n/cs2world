TAGS=with_gvisor,with_quic,with_utls,with_clash_api
VERSION=0.0.1
LDFLAGS=-s -w -X 'github.com/sagernet/sing-box/constant.Version=1.14.4-cs2world-$(VERSION)'

.PHONY: build-cs2world build-darwin-arm64 build-darwin-amd64 build-cs2world-linux build-cs2world-linux-arm64 build-cs2world-win build-all test-cs2world test clean

# 编译当前系统架构的原生内核
build-cs2world:
	go build -tags "$(TAGS)" -trimpath -ldflags="$(LDFLAGS)" -o bin/sing-box-cs2world ./cmd/sing-box-cs2world

# macOS Apple Silicon (M1/M2/M3/M4)
build-darwin-arm64:
	CGO_ENABLED=0 GOOS=darwin GOARCH=arm64 go build -tags "$(TAGS)" -trimpath -ldflags="$(LDFLAGS)" -o bin/sing-box-cs2world-darwin-arm64 ./cmd/sing-box-cs2world

# macOS Intel (x86_64)
build-darwin-amd64:
	CGO_ENABLED=0 GOOS=darwin GOARCH=amd64 go build -tags "$(TAGS)" -trimpath -ldflags="$(LDFLAGS)" -o bin/sing-box-cs2world-darwin-amd64 ./cmd/sing-box-cs2world

# Linux x86_64 (主流海外 VPS)
build-cs2world-linux:
	CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -tags "$(TAGS)" -trimpath -ldflags="$(LDFLAGS)" -o bin/sing-box-cs2world-linux-amd64 ./cmd/sing-box-cs2world

# Linux ARM64 (Oracle ARM / 树莓派 / 软路由)
build-cs2world-linux-arm64:
	CGO_ENABLED=0 GOOS=linux GOARCH=arm64 go build -tags "$(TAGS)" -trimpath -ldflags="$(LDFLAGS)" -o bin/sing-box-cs2world-linux-arm64 ./cmd/sing-box-cs2world

# Windows x86_64 (v2rayN / Nekoray 替换内核)
build-cs2world-win:
	CGO_ENABLED=0 GOOS=windows GOARCH=amd64 go build -tags "$(TAGS)" -trimpath -ldflags="$(LDFLAGS)" -o bin/sing-box-cs2world-windows-amd64.exe ./cmd/sing-box-cs2world

# 一键交叉编译全平台二进制
build-all: build-cs2world build-darwin-arm64 build-darwin-amd64 build-cs2world-linux build-cs2world-linux-arm64 build-cs2world-win

# 运行密码学单元测试 + 原生 sing-box-cs2world 端到端全链路测试
test-cs2world: build-cs2world
	go test -v ./pkg/...
	python3 scripts/test_singbox_cs2world.py

# 独立轻量端口转发器版本（可选，不依赖 sing-box）
build-standalone:
	go build -trimpath -ldflags="-s -w" -o bin/cs2-ghost ./cmd/cs2-ghost

test: test-cs2world

clean:
	rm -rf bin/
