# Turn 环境变量、安全中断与自动恢复

本文说明 `feat/turn-env-safe-interrupt` 分支新增的 App Server 接口。
适用于把 Codex App Server 接入业务系统、按请求追踪工具执行，以及暂停后自动续跑的客户端。

## 1. 功能范围

| 接口 | 新增能力 |
| --- | --- |
| `turn/start` | 接收 `env`，为工具启动的子进程设置本 turn 的环境变量 |
| `turn/steer` | 接收 `env`，替换当前 turn 后续模型步骤使用的环境变量 |
| `turn/interrupt` | 新增 `mode: "safe"`；省略时保留原来的立即中断 |
| `turn/resume` | 恢复安全暂停，或自动续跑普通中断后的工作，无需追加“继续”消息 |
| `turn/control/read` | 查询活跃 turn 的控制状态 |
| `turn/paused`、`turn/resumed` | 通知安全暂停、同 turn 恢复 |

已有的流式对话和工具调用接口继续使用原协议。这里没有增加 CLI/TUI 的暂停快捷键；客户端需要调用 App Server 接口。

## 2. 构建和启动

仓库 Rust 工具链为 `1.95.0`。在仓库根目录执行：

```bash
cd codex-rs
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 cargo build -p codex-app-server --bin codex-app-server --locked
./target/debug/codex-app-server
```

这是独立的 App Server 可执行文件，默认通过 stdin/stdout 传输逐行 JSON 消息。
沿用安装环境已有的模型、账号与 provider 配置。以下示例中的路径、thread ID 和 turn ID 都需要替换。

先执行原有的初始化握手：

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"my-client","version":"1.0"}}}
```

收到初始化响应后发送：

```json
{"jsonrpc":"2.0","method":"initialized","params":{}}
```

创建线程，并保存响应 `result.thread.id`：

```json
{"jsonrpc":"2.0","id":2,"method":"thread/start","params":{"cwd":"/path/to/workspace"}}
```

后续事件仍通过原连接流式到达。客户端应按请求 `id` 关联响应，按 `threadId`、`turnId` 关联通知；不要假设响应和通知总按固定顺序到达。

## 3. 按 turn 注入 env

### 启动

```json
{"jsonrpc":"2.0","id":10,"method":"turn/start","params":{"threadId":"THREAD_ID","input":[{"type":"text","text":"执行项目检查"}],"env":{"TRACE_ID":"trace-001","REQUEST_ID":"request-001"}}}
```

保存响应 `result.turn.id`。该 turn 的 `exec_command` 子进程可以读取 `$TRACE_ID` 和 `$REQUEST_ID`。
工具程序可以直接把它们加入自己的日志或链路追踪数据。

这里的 `env` 是在原有子进程环境之上叠加的临时变量，并不是替换整个操作系统环境。
本次接线覆盖 unified exec 的子进程启动路径；不会自动为已启动的 MCP 服务或客户端实现的 dynamic tool 注入环境。

### 运行中追加输入

```json
{"jsonrpc":"2.0","id":11,"method":"turn/steer","params":{"threadId":"THREAD_ID","expectedTurnId":"TURN_ID","input":[{"type":"text","text":"检查完成后再运行 lint"}],"env":{"TRACE_ID":"trace-002","REQUEST_ID":"request-002"}}}
```

`expectedTurnId` 必须匹配当前活跃 turn。被拒绝的请求不会更新 env。

### 生效规则

| 情况 | 行为 |
| --- | --- |
| 新 turn 省略 `env` 或传 `null` | 不继承上一 turn 的临时变量，仍使用原有基础环境 |
| 活跃 turn 的 steer 省略 `env` 或传 `null` | 保留当前临时变量 |
| 传入非空对象 | 整体替换临时变量映射，不做逐项合并 |
| 传入 `{}` | 清空临时覆盖；同名基础环境变量可能重新显现 |
| 模型步骤已经捕获快照 | 该步骤生成的工具调用继续使用旧快照 |
| 子进程已经启动 | 进程环境保持不变 |
| 多线程并发 | 各自保存变量，不修改 App Server 的全局进程环境 |
| 安全暂停后恢复 | 待执行调用保留原步骤快照，后续新步骤使用当前映射 |
| 普通中断后恢复 | 自动启动的新 turn 带上被中断 turn 保存的临时映射 |

steer 在请求被接受时替换映射。之后捕获的模型步骤使用新值；这不是“每条消息一张 env 表”。
原有 steer 机制可能触发重新采样，不保证已被抢占的模型响应还会产生工具执行。

参数限制：键和值必须为字符串，最多 64 项，所有键值的 UTF-8 字节总数不超过 65,536。
键不能为空，不能包含 `=` 或 NUL；值不能包含 NUL。
禁止覆盖 `CODEX_THREAD_ID`、`CODEX_TOOL_CALL_ID`、`CODEX_SESSION_ID`，名称比较不区分大小写。

映射保存在内存中，不作为配置写入会话历史，也不作为提示词发送给模型；相关 Debug 输出已脱敏。
如果工具主动打印这些变量，打印内容仍会成为正常工具输出，可能进入历史与模型上下文。

## 4. 安全中断

请求：

```json
{"jsonrpc":"2.0","id":20,"method":"turn/interrupt","params":{"threadId":"THREAD_ID","turnId":"TURN_ID","mode":"safe"}}
```

响应 `result: {}` 仅表示请求已接受，不代表已经暂停。
客户端需要等候 `turn/paused`，也要同时处理 turn 正常完成、失败或被立即中断的终态。

```json
{"method":"turn/paused","params":{"threadId":"THREAD_ID","turnId":"TURN_ID"}}
```

行为如下：

1. 如果主模型请求已在进行，允许这次响应结束，不取消流式响应。
2. 接受安全中断后，阻止新的工具调用进入 handler，也阻止后续主模型步骤启动。
3. 已进入 handler 的工具调用继续执行，等待其调用结束。
4. 到达可暂停边界且这些调用排空后，发送 `turn/paused`，保留待执行调用。
5. 如果模型直接给出最终答案、没有后续工作，允许 turn 正常结束，不强行制造一次暂停。

同一次暂停期间重复请求安全中断，不重复发送 `turn/paused`。
如果模型响应或已进入的工具调用长时间不结束，暂停确认也会等待；需要立即取消时，使用普通立即中断。

**安全暂停针对模型/工具调用的调度，不冻结操作系统进程。**
例如 `exec_command` 返回 session ID 后，底层长进程可能仍在运行；它可以在 turn 暂停期间继续输出或写文件。
已经运行的 tmux 任务同样不会被暂停。Code Mode 的执行容器也不是整个虚拟机冻结；新的嵌套 host 工具调用受到调度限制。

## 5. 查询状态

```json
{"jsonrpc":"2.0","id":21,"method":"turn/control/read","params":{"threadId":"THREAD_ID","turnId":"TURN_ID"}}
```

响应示例：

```json
{"id":21,"result":{"status":"paused"}}
```

| 状态 | 含义 |
| --- | --- |
| `running` | 未请求安全暂停，正常运行 |
| `pauseRequested` | 已请求暂停，尚在等待边界或工具调用排空 |
| `paused` | 已到达暂停条件 |
| `resuming` | 正在发布恢复通知，尚未放开执行 |

该接口只查询 ID 匹配的活跃普通 turn，不返回已结束 turn 的历史状态。
它可用于客户端重连后查询同一服务进程内仍然保留的活跃 turn，但不会恢复已经丢失的内存状态。
`paused` 可能在暂停通知发布前就被查询到；如果紧接着 resume 得到“暂停通知尚未发布”的错误，应等待 `turn/paused` 或重新确认状态后重试。

## 6. 恢复

### 恢复安全暂停

收到 `turn/paused` 后发送：

```json
{"jsonrpc":"2.0","id":22,"method":"turn/resume","params":{"threadId":"THREAD_ID","turnId":"TURN_ID"}}
```

返回相同 turn ID：

```json
{"id":22,"result":{"turnId":"TURN_ID"}}
```

并收到通知：

```json
{"method":"turn/resumed","params":{"threadId":"THREAD_ID","turnId":"TURN_ID"}}
```

`turn/resumed` 先进入服务端发送队列，再放开工具/模型执行。
待执行工具调用从保存的执行状态继续，不重新提交已完成调用；不添加“继续”用户消息，也不重新发出 `turn/started`。

### 恢复普通立即中断

普通中断沿用原接口，可省略 mode，也可以显式传 `"immediate"`：

```json
{"jsonrpc":"2.0","id":30,"method":"turn/interrupt","params":{"threadId":"THREAD_ID","turnId":"TURN_ID"}}
```

等待原 `turn/completed` 的 `turn.status` 为 `interrupted` 后，再调用相同的 `turn/resume`。
服务端使用已有历史自动发起新的模型步骤，不要求客户端发送用户消息。

这个路径生成**新的 turn ID**：

```json
{"id":31,"result":{"turnId":"NEW_TURN_ID"}}
```

客户端应切换到新 ID，处理原生 `turn/started`、工具事件和终态；此路径不发送 `turn/resumed`。
新 turn 的通知可以早于 resume 请求响应到达。
普通中断会取消原执行，并非保存所有执行栈；对已取消调用的后续处理由新模型步骤结合历史决定，不保证外部操作恰好执行一次。

### 失败条件和限制

- turn ID 不匹配、没有可恢复的中断记录、当前 turn 尚未安全暂停或线程已被后续任务推进时，恢复请求会报错。
- resume 不是成功后可以无限重试的幂等接口。响应丢失时，应结合通知、控制状态及原生线程状态判断，不要直接重新发送业务任务。
- 暂停控制器、待执行调用和临时 env 都没有持久化。App Server 重启、线程卸载等导致内存状态丢失后，不能通过该接口还原执行。
- `turn/resume` 不携带新的 env。安全暂停期间若需要更新后续步骤的 env，应使用现有 steer 接口；已捕获步骤仍保留旧快照。
- 原有线程访问、模型 provider 与直接输入权限检查继续生效。

## 7. 验证与代码入口

在仓库根目录运行：

```bash
node scripts/verify-turn-controls.mjs
```

默认使用 `codex-rs/target/debug/codex-app-server`，可通过 `CODEX_FORK_BINARY` 指定其他构建。
脚本启动真实 App Server 和真实 shell，用本地受控 Responses 服务代替模型推理，不调用真实模型、不需要 API key。
它不是模型能力测试，也没有验证远端 exec-server 环境的实际连通性。

覆盖七个场景：

1. start/steer 的 env 更新、拒绝过期 steer、步骤快照、新 turn 清空，以及 env 值不进入模型输入。
2. 并发线程 env 隔离。
3. 工具执行前安全暂停、重复暂停通知去重、同 turn 恢复和单次副作用。
4. 已进入 handler 的调用结束后才确认暂停。
5. 普通中断后无新增用户消息自动续跑，保留 env，并拒绝过期恢复。
6. 安全暂停不冻结已启动的后台进程。
7. 普通立即中断可以结束已安全暂停的 turn。

脚本输出临时目录内的 `report.json` 路径，并保留通知记录和服务端 stderr 供检查。
本分支还通过了 7 项定向 Rust 单测、App Server 构建和 `cargo check -p codex-cli --locked`。

| 代码 | 职责 |
| --- | --- |
| [协议定义](../../codex-rs/app-server-protocol/src/protocol/v2/turn.rs) | 请求、响应和通知类型 |
| [App Server 处理器](../../codex-rs/app-server/src/request_processors/turn_processor.rs) | 接口接线、通知发布与恢复放行 |
| [暂停控制器](../../codex-rs/core/src/turn_control.rs) | 模型/工具准入、排空、暂停与恢复状态 |
| [Core 线程入口](../../codex-rs/core/src/codex_thread.rs) | 按 turn ID 检查与恢复 |
| [子进程环境接线](../../codex-rs/core/src/unified_exec/process_manager.rs) | 将步骤 env 快照传入 exec 启动路径 |
| [进程回归脚本](../../scripts/verify-turn-controls.mjs) | 可复现的七项端到端验证 |

稳定版、实验版协议快照以及 Python SDK 生成类型已同步。后续修改协议时，应重新生成快照，再构建交付二进制，因为快照通过 `include_bytes!` 编入可执行文件。

```bash
uv run --no-project python codex-rs/app-server-protocol/scripts/write_schema_fixtures.py
uv run --no-project python codex-rs/app-server-protocol/scripts/write_schema_fixtures.py --experimental
```
