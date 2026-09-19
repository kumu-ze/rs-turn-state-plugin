# RS Turn-State 插件（迁移预览）

独立 Rust 可执行程序，不依赖 RS 的内部 crate。策略合同与票复用规则提取自
kumu-ze/codex-proxy-rs 的 8ab9101f，保留 Apache-2.0 许可。

当前支持：旧数据离线导入、身份/账号/模型/有效期校验、按策略替换状态、无票拦截、只读页面及本地倒计时。
**尚不支持生成新票、自动/持续探测、出口采样和在线策略编辑。票到期后无法补票，不能替换现有生产打票服务。**
旧代理和日志数据在导入文件中保留，但本版本不执行探测，也不在页面暴露代理认证或票正文。

## 构建与安装

宿主需要 codex/plugin-host 分支的扩展上下文字段：authenticationKind、planType、accountEligible。
先 `cargo build --release --locked`，再运行：

```sh
python3 scripts/package.py target/release/rs-turn-state-plugin dist/turn-state-0.1.0
codex-proxy-rs plugin-install dist/turn-state-0.1.0 /path/to/plugins
mkdir -p /path/to/plugin-data/turn-state
target/release/rs-turn-state-plugin import /path/to/backup/turn-state-tickets.json /path/to/plugin-data/turn-state/turn-state-tickets.json
```

仅在隔离环境使用旧文件的备份。导入保留源文件且拒绝覆盖目标；只迁移有 auth_binding 且未过期的有效格式票。
这些校验不代表已验证票的上游有效性。没有身份摘要的历史票不能靠 revision 在不同宿主之间复用。
目标父目录必须存在，导入失败不会留下一份可被误读的部分文件。

宿主配置：

```yaml
plugins:
  - directory: /path/to/plugins/turn-state-0.1.0
    data_directory: /path/to/plugin-data/turn-state
```

重启后加载插件，管理端「插件」可打开只读页面。数据只在启动时读取；修改或再次导入前先停用插件。
回退时移除配置并重启，数据保留。此版本不修改原实例数据库或生产数据。

## RPC

使用宿主 API v1 的 4 字节大端长度前缀 JSON-RPC；最大帧 1 MiB。
initialize 校验固定插件 ID turn-state；request.before_send 返回 deny 和 session_state；
admin.panel 只返回设置及票的账号、模型、到期时间；admin.ui 返回隔离页面。
未知方法返回业务错误，宿主应继续保留进程。

验证：`cargo test --locked`。真实上游探测与 HTTP/WS 全链路迁移仍待完成。
