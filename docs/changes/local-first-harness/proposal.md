# Proposal

- **Status:** accepted
- **Owner:** project owner
- **Why:** 将当前调研结论落为一个本地优先、事件驱动且可替换执行环境的 coding-agent runtime，而不是继续堆叠入口或进程内插件。
- **What changes (user-facing):** 新建 Rust workspace 作为 daemon 核心，定义稳定 versioned JSON-RPC/event protocol；以 SQLite + append-only event log 驱动 session projection、replay/resume/fork；通过 PolicyEngine 和 ExecutionBroker 约束文件与进程工具；提供 TypeScript SDK 和可运行的 headless/stdio 客户端 seam。
- **Out of scope:** 默认联网浏览、长期记忆、自我改写技能、IM 网关、`--yolo` 全权限模式、任意进程内插件、远程/VM 执行的真实实现。容器/远程执行只保留明确 adapter seam。
- **Risks / rollback:** Rust 工具链当前未安装，无法在本机编译 Rust；先以协议契约、TypeScript SDK 测试和源码静态检查提供证据，安装 Rust 后再补 cargo 验证。所有事件写入 append-only 表，实验性 schema 通过 protocol version 隔离；回滚只删除新建目录/提交，不覆盖原调研文件。

## Acceptance

- [ ] 同一事件序列可稳定 derive 出相同 model history，并可 replay。
- [ ] session 断开后可从 SQLite 恢复，fork 从指定 event sequence 开始生成新 session。
- [ ] 工具副作用必须经历 proposal、approval、started、finished/failed 事件。
- [ ] path 与 process policy 共用 workspace roots，拒绝越界与命令策略外执行。
- [ ] daemon/SDK 只依赖 versioned JSON-RPC，不暴露内部 agent 对象。
- [ ] 每个核心行为都有针对性的自动化测试或明确记录验证阻塞。
