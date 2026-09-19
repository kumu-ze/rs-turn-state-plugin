# RS Turn-State 插件

独立 Rust 进程插件，版本 0.2.0。业务源码提取自 kumu-ze/codex-proxy-rs 的 8ab9101f，保留 Apache-2.0 许可。无需依赖或编译 RS 内部 crate。

## 功能

- 手动探测、按周期自动补票、仅业务活跃时补票、持续探测及命中自动停止。
- 代理池轮换、入口启停与并发、导入宿主已有代理、出口 IP 采样。
- 账号/模型/认证身份隔离、票复用、无票保护、目标长度和有效期策略。
- 策略与票原子持久化、历史日志和清理、限流退避、旧数据导入。
- 独立 Vue 管理页面，保留原页面功能及实时倒计时；通过 sandbox iframe 消息桥接访问插件。

宿主提供账号快照、固定目标的 Provider 网络探测及代理读取；OAuth 凭据留在宿主。插件拥有打票策略、后台调度、票和日志数据。原生插件必须由管理员信任，并非操作系统沙箱。

## 构建和安装

需要支持 provider.openai 能力的 codex/plugin-host 分支。旧的 0.1.0 宿主不具备完整插件服务接口。

```sh
cd frontend
pnpm install --frozen-lockfile --config.auto-install-peers=false
pnpm build
cd ..
cargo build --release --locked
python3 scripts/package.py target/release/rs-turn-state-plugin dist/turn-state-0.2.0
codex-proxy-rs plugin-install dist/turn-state-0.2.0 /path/to/plugins
mkdir -p /path/to/plugin-data/turn-state
```

可选：在停止插件后从旧文件的备份导入，目标文件必须不存在：

```sh
target/release/rs-turn-state-plugin import /path/to/backup/turn-state-tickets.json /path/to/plugin-data/turn-state/turn-state-tickets.json
```

源文件保留；策略、代理、日志及退避数据保留，票只复用具有身份摘要且未过期的有效格式记录。上游有效性仍由实际调用决定。新建插件没有文件时默认关闭。

宿主配置：

```yaml
plugins:
  - directory: /path/to/plugins/turn-state-0.2.0
    data_directory: /path/to/plugin-data/turn-state
```

重启宿主后，在「插件」打开 Turn-State 页面，检查账号和策略再启用。不要同时运行旧内置打票和新插件的自动任务。停用时移除配置并重启；插件数据保留。回滚使用切换前的宿主与数据备份。

## 验证

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python3 tests/workflow.py target/release/rs-turn-state-plugin
```

workflow.py 使用真实插件进程及模拟宿主服务，覆盖保存、手动打票、身份变化拦截、代理导入、出口采样、日志清理、持续并发、按需自动和重启恢复，不访问生产账号。

宿主安装测试源在 tests/host-integration.rs.disabled，供隔离的 gateway-host 测试目录加载；设置 RS_TURN_STATE_PACKAGE 后执行 ignored 测试。宿主还分别验证能力令牌鉴权、探测凭据过滤和 HTTP/WS 状态替换。

本版本已完成上述功能提取和隔离测试；尚未在生产账号完成完整上线验收，未测量生产吞吐差异。不要将模拟工作流测试视为生产部署成功。
