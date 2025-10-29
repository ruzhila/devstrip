.PHONY: all build release test clean run install fmt clippy check help

# 默认目标
all: build

# 构建 debug 版本
build:
	cargo build

# 构建 release 版本
release:
	cargo build --release

# 运行测试
test:
	cargo test

# 清理构建产物
clean:
	cargo clean

# 运行程序
run:
	cargo run

# 安装到系统
install:
	cargo install --path .

# 格式化代码
fmt:
	cargo fmt

# 代码检查
clippy:
	cargo clippy -- -D warnings

# 完整检查（格式化 + clippy + 测试）
check: fmt clippy test

# 构建并运行
dev:
	cargo run -- --help

# 显示帮助信息
help:
	@echo "devstrip Makefile 命令："
	@echo "  make build    - 构建 debug 版本"
	@echo "  make release  - 构建 release 版本"
	@echo "  make test     - 运行测试"
	@echo "  make clean    - 清理构建产物"
	@echo "  make run      - 运行程序"
	@echo "  make install  - 安装到系统"
	@echo "  make fmt      - 格式化代码"
	@echo "  make clippy   - 代码检查"
	@echo "  make check    - 完整检查（fmt + clippy + test）"
	@echo "  make dev      - 快速运行开发版本"
	@echo "  make help     - 显示此帮助信息"
