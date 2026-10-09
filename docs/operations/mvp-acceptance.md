# Coding-agent MVP 验收记录

## 已运行验证

- Rust workspace 离线测试全部通过，包括公共 daemon stop 集成测试。
- Node 客户端/网关测试 24/24 通过；TypeScript noEmit 检查通过。
- Docker 当前源码镜像可成功构建，默认 loopback 模式无 token 的健康 RPC 可用。
- 真实 gateway HTTP + 本地 OpenAI-compatible fixture（不是外部收费模型）通过以下流程：
  1. 保存 provider 配置并创建工作区 session；
  2. 模型提出 read_file，daemon 自动执行并将真实正文反馈模型；
  3. edit_file 经精确审批修改文件；
  4. test `/usr/bin/false` 执行并得到失败、退出码 1；
  5. 模型收到失败状态后提出修复编辑，审批后执行；
  6. test `/usr/bin/true` 得到成功、退出码 0；
  7. 模型最终回答，持久事件记录 RunCompleted(success=true)，实际文件内容为 fixed；
  8. 新回合能从历史中读取上一回合的文件正文；
  9. 在 fixture 阻塞模型响应时 stop RPC 即时返回 accepted=true/stopped=false，响应放行后没有 ToolProposed，RunCompleted(success=false)。

验收使用真实 Docker 镜像、Node gateway、stdio daemon、SQLite、文件编辑和进程执行；只有模型输出是确定性 fixture。临时容器在验收结束后停止。

## 启动与执行

```bash
pnpm fnfyuh web
```

打开 http://127.0.0.1:8787，在设置中配置自己的 provider/model。运行编译、测试等命令前，按 README 显式开启受信进程执行，并为容器内存在的程序配置绝对路径列表；默认仍需批准副作用。Docker runtime 不附带所有项目的工具链，须确保选用运行环境具备项目需要的程序。

## 明确边界

- 停止是协作式停止：确认请求后，当前模型请求/工具步骤结束才停止后续动作；不是立即杀进程或撤销已经发生的文件修改。
- 正常 RPC 仍串行处理。执行期间 SSE pull/历史查询可能等待，UI 的运行提示不等于实时 token streaming。
- 文件操作记录只是成功写入/编辑摘要，不是完整 Git diff；提交前需自行检查 Git diff。
- 真实 Chrome headless 已通过设置添加并保存模型、新建会话、发送任务、工具审批、最终回答、多会话刷新恢复、工具正文恢复、文件操作记录恢复及停止按钮验收；Runtime exception 为 0。修复了设置读取覆盖用户修改、审批凭据未到达时按钮可点、以及多会话刷新丢失选中状态。自动化验证 DOM 和真实交互，不等于对所有屏幕尺寸的视觉审查。
- 未使用真实 provider 凭据；外部模型与 OAuth 的网络/账号可用性未在本次验收验证。
