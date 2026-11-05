# Turn 环境变量与中断后自动续跑

本文说明本分支为 Codex App Server 增加的两个能力：按 turn 传递工具进程环境变量，以及在普通中断后继续工作。

| 接口 | 能力 |
| --- | --- |
| `turn/start` | 接收 `env`，为该 turn 启动的工具与 command hook 子进程设置变量 |
| `turn/steer` | 接收 `env`，替换当前 turn 后续模型步骤使用的变量 |
| `thread/queue/add` | 接收 `env`，随排队输入保存，在该输入启动新 turn 时应用 |
| `turn/interrupt` | 沿用原有立即中断 |
| `turn/resume` | 基于已有历史启动新的 turn，无需发送“继续”消息 |

## 构建与启动

从仓库根目录运行：

```bash
cd codex-rs
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 cargo build -p codex-app-server --bin codex-app-server --locked
./target/debug/codex-app-server
```

App Server 默认通过 stdin/stdout 传输逐行 JSON，沿用已有的模型、账号和 provider 配置。以下示例的路径与 ID 需要替换。

初始化并创建线程：

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"my-client","version":"1.0"}}}
```

```json
{"jsonrpc":"2.0","method":"initialized","params":{}}
```

```json
{"jsonrpc":"2.0","id":2,"method":"thread/start","params":{"cwd":"/path/to/workspace"}}
```

先等待 initialize 响应，再发送 initialized 和 thread/start；线程 ID 取自 `result.thread.id`。后续按请求 `id` 关联响应，按 `threadId`、`turnId` 关联事件。

## 传递环境变量

启动 turn：

```json
{"jsonrpc":"2.0","id":10,"method":"turn/start","params":{"threadId":"THREAD_ID","input":[{"type":"text","text":"执行项目检查"}],"env":{"TRACE_ID":"trace-001","REQUEST_ID":"request-001"}}}
```

turn ID 取自 `result.turn.id`。该 turn 的 `exec_command` 子进程可以读取 `$TRACE_ID` 和 `$REQUEST_ID`。

运行中追加输入并更新变量：

```json
{"jsonrpc":"2.0","id":11,"method":"turn/steer","params":{"threadId":"THREAD_ID","expectedTurnId":"TURN_ID","input":[{"type":"text","text":"检查完成后运行 lint"}],"env":{"TRACE_ID":"trace-002","REQUEST_ID":"request-002"}}}
```

`expectedTurnId` 必须匹配当前活跃 turn；被拒绝的 steer 不改变环境变量。临时变量叠加在子进程基础环境之上，行为如下：

| 请求或状态 | 行为 |
| --- | --- |
| 新 turn 省略 `env` 或传 `null` | 临时映射为空，不继承上一 turn 的映射 |
| steer 省略 `env` 或传 `null` | 保留当前临时映射 |
| 传入对象 | 整体替换临时映射；`{}` 清空覆盖，同名基础变量可能重新显现 |
| 模型步骤已捕获快照 | 该步骤的工具使用旧快照；之后捕获的步骤使用更新后的映射 |
| 工具的 PreToolUse、PostToolUse hook | 使用对应工具步骤捕获的 env 快照，steer 不改变已有步骤 |
| 模型请求 hook、工具审批的 PermissionRequest hook | 使用发起请求的步骤快照 |
| UserPromptSubmit、Stop、Interrupt 等 turn 级 hook | 使用触发时的当前 turn env；异步 hook 保留调度时的快照 |
| 子进程已启动 | 进程已有环境不会改变 |
| 多个线程并发 | 各 turn 独立保存映射，不修改 App Server 全局环境 |
| 普通中断后恢复 | 新 turn 使用被中断 turn 的临时映射 |

键和值必须为字符串，最多 64 项，键值 UTF-8 字节总数不超过 65,536。键不能为空，不能包含 `=` 或 NUL；值不能包含 NUL。禁止覆盖 `CODEX_THREAD_ID`、`CODEX_TOOL_CALL_ID`、`CODEX_SESSION_ID`，名称比较不区分大小写。

本地 command hook 的基础环境来自 App Server 启动环境，远程 command hook 的基础环境来自对应执行环境的 shell 策略；之后叠加 turn env，hook 内部设置的变量优先，仍执行原有受限变量过滤。工具 shell 中临时执行的 `export` 不会回传给后续 hook；需要传递的 trace 变量应通过上述 `env` 字段设置。

`turn/start` 和 `turn/steer` 的映射只保存在内存中；排队输入的映射随队列项持久化，启动后从队列移除。映射不作为配置写入会话历史或提示词，Debug 输出对映射脱敏。如果工具主动打印变量，其输出仍可能进入历史和模型上下文。接线覆盖 unified exec、在本地或远程环境运行的 command hook。legacy notify、MCP hook、已启动的 MCP 服务和客户端 dynamic tool 不会自动接收此映射；没有关联 turn 的 SessionEnd hook 不继承上一 turn 的映射。

### 排队输入

初始化客户端时需声明 `capabilities.experimentalApi: true`，沿用原生队列接口的要求：

```json
{"jsonrpc":"2.0","id":12,"method":"thread/queue/add","params":{"threadId":"THREAD_ID","input":[{"type":"text","text":"运行下一项检查"}],"clientUserMessageId":"queued-message-001","env":{"TRACE_ID":"trace-queued"}}}
```

入队不改变活跃 turn 的 env。自动出队和 `thread/queue/start` 都将该队列项的 env 应用到新 turn 的工具和 command hook；省略或传 `null` 表示新 turn 无临时覆盖，`{}` 也表示空覆盖，不继承前一 turn。`thread/queue/update` 只编辑输入并保留该项 env。入队时使用相同的 env 校验规则，非法 env 不写入队列；旧版队列项按无 env 读取。add/list 等响应仍保持原有结构，不回显 env。

## 立即中断后续跑

发送原有中断请求：

```json
{"jsonrpc":"2.0","id":20,"method":"turn/interrupt","params":{"threadId":"THREAD_ID","turnId":"TURN_ID"}}
```

等待原 turn 的 `turn/completed` 事件中 `turn.status` 为 `interrupted`，然后请求续跑：

```json
{"jsonrpc":"2.0","id":21,"method":"turn/resume","params":{"threadId":"THREAD_ID","turnId":"TURN_ID"}}
```

响应中的 `result.turnId` 是**新的 turn ID**。服务端基于已有历史自动启动模型步骤，客户端不需要发送“继续”消息。客户端应使用新 ID 跟踪原生 `turn/started`、工具事件和终态；新 turn 的事件可以早于 resume 响应。

普通中断会取消原执行，不保存完整执行栈。恢复后模型依据已有历史决定接下来的动作，不能保证外部操作恰好执行一次。turn ID 不匹配、没有可恢复记录或线程已经推进时，恢复请求会报错；响应丢失时应先检查线程状态，避免盲目重试。

被中断 turn 的恢复记录和临时 env 只在当前 App Server 内存中。服务重启或线程卸载导致状态丢失后，不能通过该接口自动恢复；`turn/resume` 本身不接收新的 env。原有线程访问、模型 provider 和直接输入权限检查继续生效。

## 验证与代码

在仓库根目录运行：

```bash
node scripts/verify-turn-controls.mjs
```

脚本默认使用 `codex-rs/target/debug/codex-app-server`，可用 `CODEX_FORK_BINARY` 覆盖。它启动真实 App Server 和 shell，以本地受控 Responses 服务代替模型推理，不调用真实模型或消耗 API key。脚本检查：

1. start/steer 的 env、旧步骤快照、过期 steer 拒绝，以及新 turn 清空映射。
2. 并发线程环境变量隔离。
3. 普通中断后自动续跑、保留 env、不添加“继续”消息，以及拒绝再次恢复旧 turn。

脚本同时检查本地 `UserPromptSubmit`、`PreToolUse`、`PostToolUse`、`Stop`、`Interrupt` command hooks 对 turn env 和基础环境的读取、线程隔离，以及异步 hook 的 env 快照。

另外两项队列回归覆盖自动派发、更新输入保留 env、非法 env 拒绝、队列项隔离，以及中断后指定队列项启动。队列服务测试覆盖 env 保存后重新加载、旧记录兼容及编辑保留 env。

脚本输出临时目录内的 `report.json` 路径，并保留通知记录和服务端 stderr。它不验证真实模型能力或远端 exec-server 连接；实际结果以本次运行报告为准。

代码入口：[协议定义](../../codex-rs/app-server-protocol/src/protocol/v2/turn.rs)、[App Server 处理器](../../codex-rs/app-server/src/request_processors/turn_processor.rs)、[Core 线程入口](../../codex-rs/core/src/codex_thread.rs)、[子进程环境接线](../../codex-rs/core/src/unified_exec/process_manager.rs)、[进程回归脚本](../../scripts/verify-turn-controls.mjs)。

修改协议后，需要重新生成稳定版和实验版协议快照，再构建交付二进制：

```bash
uv run --no-project python codex-rs/app-server-protocol/scripts/write_schema_fixtures.py
uv run --no-project python codex-rs/app-server-protocol/scripts/write_schema_fixtures.py --experimental
```
