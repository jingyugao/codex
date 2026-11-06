# Codex fork 交接文档

更新时间：2026-11-06。分支 `feat/fork-v1`（仓库 `jingyugao/codex`，上游 `openai/codex`）。

## 1. 目标与原则

本 fork 不合并回上游，但要持续拉取上游更新，所以**对上游文件的改动越少越好**：

- 新代码放在新 crate（`codex-rs/fork/`）或新文件（`core/src/fork_env.rs`）。
- 上游文件只保留几行接入点，统一用 `// FORK:` 注释标出，rebase 时 `git grep -n "FORK:"` 即可全部找到。
- 不改协议定义，也不改生成文件（schema JSON、TypeScript、Python SDK）。

三个需求：

| 需求 | 实现 |
|---|---|
| 1. 按 turn 传 env（trace 等）给 hook / exec | `core/src/fork_env.rs` + 接入点 |
| 2. 对话数据存 PostgreSQL，本地不存 | `codex-rs/fork/pg`（`codex-fork-pg`） |
| 3. 安全 interrupt：A pod 上的 turn 停下，迁到 B pod 继续 | `codex-rs/fork/ext`（`codex-fork-ext`），v1：滚动发布时用 |

工作区文件（remote sandbox）由其它模块负责，不在本 fork 范围内。

## 2. 分支与提交

- `feat/fork-v1`：从上游 `7b54fd2` 切出，在其上的提交：
  - `6a20465` / `ae41678`：fork 的 GitHub Actions 构建与测试
  - `d3f68b4`：per-turn env
  - `0ff0e74`：PG thread store + 跨 server 交接
  - `f354156` / `19b432a`：构建脚本
  - `1ba5814`：thread/resume 不再要求本地 rollout 文件（PG 模式跨 pod 恢复必需）
- `main_backup`（本地与远端）：旧 `fork/main` 的备份（早期 PR #1、#2 的实现，已被本方案取代）。
- 远端 `fork/main` 尚未改动，仍是旧实现。是否重置成上游需要另行确认（需要 force push）。

**禁止把编译产物提交进 git。** 二进制走 GitHub Release（见第 6 节）。

## 3. 功能说明

### 3.1 per-turn env

客户端在 `turn/start` 的已有字段 `responsesapiClientMetadata` 里传 `fork.env.<NAME>`：

```json
{"method":"turn/start","params":{"threadId":"…","input":[…],
  "responsesapiClientMetadata":{"fork.env.TRACE_ID":"abc123"}}}
```

- 去掉前缀后作为环境变量注入：本地 command hook、远程 environment hook、`exec_command`。
- 这些条目在发给模型 provider 和 MCP server 之前会被剔除。
- 校验（fork 加的，`app-server/src/fork_metadata.rs`）：`turn/start`、`turn/steer`、`thread/queue/add` 中的 `fork.env.*` 条目最多 16 个，key 含前缀不超过 64 字节，变量名必须匹配 `[A-Za-z_][A-Za-z0-9_]*`，值不超过 128 字节且不含 NUL。不符合时返回 `invalid_request`。其他 metadata key 不做检查，和上游一致（上游对每轮 metadata 不做长度校验）。
- 并入活动 turn 时（`turn/start` 或 `turn/steer`），整份 metadata 被替换，之后启动的 exec 和 hook 使用新 env。
- 跨 pod 恢复的 turn 没有 client metadata，env 通过 `ForkTurnEnv`（turn extension data，按 turn_id 生效）带过去。
- 需要客户端开启 `experimentalApi`。

排队消息同样可以带 env：`thread/queue/add` 新增可选字段 `responsesapiClientMetadata`，语义与 `turn/start` 相同。

```json
{"method":"thread/queue/add","params":{"threadId":"…","input":[…],"clientUserMessageId":"u1:…",
  "responsesapiClientMetadata":{"fork.env.COLONY_USER":"u1"}}}
```

- 和输入一起存进队列记录，出队（`thread/queue/start` 或空闲自动派发）时作为该 turn 的 metadata。
- 存储格式：没有 metadata 时仍然是上游的 `TurnInput` JSON；有 metadata 时包一层 `{"forkQueuedInputV1":{"input":…,"responsesapiClientMetadata":…}}`（`ext/queue/src/fork_env.rs`）。
- `thread/queue/update` 保留原来的 metadata；`thread/queue/list` 不返回 metadata。
- env 以明文存在本地 sqlite 队列库中，出队或删除时随记录删除。

### 3.2 PostgreSQL thread store

`config.toml`：

```toml
experimental_thread_store = { type = "postgres", url = "postgres://user:pass@host:5432/db" }
# 或省略 url，改用环境变量 CODEX_FORK_PG_URL
```

- 实现上游的 `ThreadStore` trait（16 个必需方法），表结构见 `codex-rs/fork/pg/migrations/0001_init.sql`，首次使用时自动迁移。
- 所有权防护：每个 thread 有 `owner_id` + `epoch`。resume 时 `epoch+1`，旧 owner 的写入返回 `Conflict`，防止两个 pod 同时写。
- URL 无效时启动直接 panic，**不会退回本地存储**。
- 未实现（返回 Unsupported 或空）：sections、attachments、projects、全文搜索、按路径读、paginated 历史。
- 排队（`thread/queue/*`）：使用本机 sqlite 队列库（与原生模式相同的 `LocalQueueStore`），不在 PG 中。队列只对本 pod 可见；`CODEX_SQLITE_HOME` 在 emptyDir 上时，pod 重建后队列丢失，在 PVC 上则保留。删除 PG thread 时不会清理它的队列项。
- 本地仍有的文件：app-server 自身的 sqlite（日志等）。部署时建议：`history.persistence = "none"`，`CODEX_SQLITE_HOME` 指向 emptyDir，控制日志级别。

### 3.3 跨 pod 交接（v1）

流程：

1. A 收到 SIGTERM（或 NOTIFY）后，先把交接记录写进存储（turn_id、environment 选择、per-turn env），再调用上游的 `suspend_turn_and_shutdown`：flush 历史，停止 turn，但不写终止事件。
2. 上游的 graceful drain 看到没有运行中的 turn，A 正常退出。
3. 客户端连到 B 并调用 `thread/resume`。B 在 idle 回调里认领记录，调用上游的 `recover_turn_if_idle`，以**同一个 turn_id** 继续执行。

v1 是"立即停"：进行中的工具调用会被中断，B 上模型看到中断后自行决定是否重试。所以只适合滚动发布、挑空闲时段的场景。"等到 step 边界再停"（v2）未实现。

环境变量（不改 config schema）：

| 变量 | 含义 |
|---|---|
| `CODEX_FORK_HANDOFF_STORE` | `postgres://…` 或 `file:<dir>`；不设则交接功能关闭 |
| `CODEX_FORK_HANDOFF_ON_SIGTERM` | `1`：SIGTERM 后经过宽限期，挂起仍在运行的 turn |
| `CODEX_FORK_HANDOFF_GRACE_SECS` | 宽限期，默认 30，让 turn 有机会自然结束 |
| `CODEX_FORK_OWNER_ID` | 本进程身份，默认取 `HOSTNAME` |

单个 thread 迁移（仅 Postgres store）：`NOTIFY codex_fork_handoff, '<thread_id>'`，加载着该 thread 的 pod 会挂起它。断线期间的通知会丢失，控制器需要重试，直到 owner 变化。

注意：

- 只在 WebSocket 模式下生效。stdio 模式下上游不做 graceful drain。
- 只有根 thread 的普通 turn 可以迁移。子 agent 会被中断，不做恢复。
- remote environment 的配置不随交接迁移：B 必须能识别同一个 environment_id（`environments.toml` 或 `environment/add`）。
- k8s 的 `terminationGracePeriodSeconds` 要大于 `CODEX_FORK_HANDOFF_GRACE_SECS` 加挂起所需时间。

## 4. 对上游文件的改动（rebase 冲突点）

`git diff 7b54fd2 HEAD --stat -- . ':!codex-rs/fork'`。除了 `fork/`、`fork_env.rs`、`scripts/fork/`、workflow 之外：

| 文件 | 改动 |
|---|---|
| `codex-rs/Cargo.toml` | workspace 成员与依赖各 +2 行 |
| `core/src/lib.rs` | 注册 `fork_env` |
| `core/src/turn_metadata.rs` | 快照访问器；外发 metadata 剔除 `fork.env.*` |
| `core/src/hook_environment_executor.rs` | hook 注入 env（本地 + 远程） |
| `core/src/unified_exec/process_manager.rs` | exec 注入 env |
| `hooks/src/registry.rs`、`hooks/src/engine/command_runner.rs` | `with_command_env` |
| `config/src/config_toml.rs`、`core/src/config/mod.rs`、`core/src/thread_store_builder.rs` | `postgres` store 配置 |
| `app-server/src/message_processor.rs` | Postgres 模式使用本地 sqlite queue store |
| `app-server/src/lib.rs` | 注册 `fork_metadata` |
| `app-server/src/request_processors/turn_processor.rs` | `turn/start`、`turn/steer` 校验 `fork.env.*` |
| `app-server/src/request_processors/thread_queue_processor.rs` | `queue/add` 校验并保存 metadata |
| `app-server-protocol/src/protocol/v2/thread.rs` | `ThreadQueueAddParams.responsesapiClientMetadata` |
| `ext/queue/src/service.rs`、`ext/queue/src/lib.rs`、`ext/queue/Cargo.toml` | 队列项 metadata 的存取与出队应用 |
| `app-server/tests/suite/v2/*.rs`、`tui/src/session_queue_commands.rs` | 结构体字面量补 `responsesapi_client_metadata: None` |
| `app-server/src/extensions.rs` | 安装 fork extension（1 行） |
| `app-server/src/request_processors/thread_processor.rs` | resume 允许没有 rollout path |

依赖的上游能力：`ThreadStore` trait、`CodexThread::suspend_turn_and_shutdown` / `recover_turn_if_idle` / `restorable_thread_settings`、extension API（`ThreadLifecycleContributor`）、app-server graceful drain。上游改了这些接口时，fork crate 会**编译失败**而不是产生 git 冲突，按报错跟进即可。

拉上游：`git fetch origin && git rebase origin/main`（建议开启 `git config rerere.enabled true`）。之后跑第 5 节的测试。

## 5. 构建与测试

### 机器资源

codex-rs 很大：175 个 crate，`codex-core` 单 crate 约 27 万行，debug 版 app-server 约 900 MB。并行度要按内存来，**每个 job 约 3 GB**。15 GB 的机器用 `-j 2`，否则会内存耗尽。本机资源不足时，到远程机器构建，或交给 GitHub Actions。

### 构建脚本

`scripts/fork/build.sh`：

```sh
# 全新机器：克隆并构建
curl -fsSL https://raw.githubusercontent.com/jingyugao/codex/feat/fork-v1/scripts/fork/build.sh \
  | bash -s -- --install-deps --ref feat/fork-v1
# 已有源码目录（无需 git）
scripts/fork/build.sh --source . --install-deps
```

- 默认构建 release 版 `codex` 和 `codex-exec-server`。`codex app-server` 是 `codex` 的子命令。
- 可选参数：`--jobs N`、`--profile debug`、`--prefix DIR`、`--with-code-mode`（需要下载 V8）。
- 产物是本机原生动态链接版：依赖构建机的 glibc 版本，以及 `libssl.so.3`。

### 测试

```sh
docker run -d --name codex-fork-pg -p 127.0.0.1:55432:5432 \
  -e POSTGRES_PASSWORD=codex -e POSTGRES_DB=codex postgres:16-alpine
cd codex-rs
cargo build -j 2 -p codex-app-server --bin codex-app-server
CODEX_FORK_PG_TEST_URL=postgres://postgres:codex@127.0.0.1:55432/codex \
  cargo test -j 2 --no-fail-fast -p codex-fork-pg -p codex-fork-ext
```

用指定二进制（比如 release 产物）跑端到端测试：

```sh
env "CARGO_BIN_EXE_codex-app-server=/path/to/codex-app-server" CODEX_FORK_PG_TEST_URL=… \
  cargo test -j 2 -p codex-fork-ext --test handoff_e2e
```

只有 `codex` 而没有 `codex-app-server` 时，写一个包装脚本：`exec /path/codex app-server "$@"`。

| 测试 | 覆盖 |
|---|---|
| `fork/pg/tests/pg_store.rs` | 与 InMemory 行为对齐的往返、列表/归档/删除、旧 owner 被拒、交接只被认领一次 |
| `fork/ext/tests/handoff_e2e.rs` 用例 1 | file store：SIGTERM → 同一 turn_id 恢复 → B 上 `echo $TRACE_ID` 输出带 env |
| `fork/ext/tests/handoff_e2e.rs` 用例 2 | 两个独立 CODEX_HOME 只共享 PG：恢复成功，且两边都没有对话文件 |
| `core` 中 `fork_env` 单测 | 前缀与变量名过滤；`fork.env.*` 校验 |
| `ext/queue/tests/fork_queue_env.rs` | 队列项 metadata：`queue/start` 和空闲派发都应用到 turn env；update 保留；list 不暴露 |

当前状态：以上测试在 `1ba5814` 上，用本地 debug 构建全部通过。

### CI（`.github/workflows/fork-release.yml`）

- push 到 `feat/fork-**` 分支时触发：
  - `build`：musl 静态 release 包，产物作为 artifact 上传。
  - `fork-tests`：带 Postgres service 跑 fork 测试。
- 发布 GitHub Release 只在手动触发时进行：`workflow_dispatch` 且 `publish=true`。
- 首次运行没有缓存，或 `Cargo.lock` 变化后，需要 1 小时以上。
- 截至 `f354156` 的两次 CI 中，`fork-tests` 都失败在 env 断言：runner 上没有 bubblewrap，命令被沙箱拦截。`1ba5814` 已把测试改为 `danger-full-access`，待新一轮 CI 确认。

## 6. 发布流程（二进制）

1. 远程机器从 git 拉取 `feat/fork-v1`，用构建脚本构建。
2. 打包后上传到 **draft** release，**不要提交 git**：

   ```sh
   cd codex-rs/target/release
   tar -czf codex-linux-x86_64.tar.gz codex codex-exec-server
   sha256sum codex-linux-x86_64.tar.gz > SHA256SUMS
   read -rs GH_TOKEN && export GH_TOKEN     # 细粒度 token：仅本仓库，Contents 读写
   gh release create fork-<短sha> -R jingyugao/codex --draft --prerelease \
     --title "Codex fork <短sha> (Linux x86_64)" --notes "Built from <sha>, glibc <版本>" \
     codex-linux-x86_64.tar.gz SHA256SUMS
   unset GH_TOKEN
   ```

3. 验证方下载 draft 中的文件，校验 sha256，按第 5 节对该二进制跑端到端测试。
4. 通过后把 tag 指向对应 commit，再发布：`gh release edit … --target <sha> --draft=false`。

## 7. 未完成 / 后续

- [ ] 用 `19b432a` 的远程构建产物重新验证，然后发布 Release（旧的预编译包基于 `f354156`，缺少 PG resume 修复，不应发布）。
- [ ] 确认新一轮 CI 通过。
- [ ] 远端 `fork/main` 的去留（旧实现；重置需要 force push）。
- [ ] 二期：PG 持久化的 QueueStore（目前 PG 模式下用本地 sqlite 队列）。
- [ ] v2：在 step 边界挂起（需要改 core turn loop，冲突风险中等）；等 v1 实际使用后再决定。
- [ ] B 恢复时让 remote environment 自动可用（目前依赖 B 端预先配置）。
- [ ] 本地 sqlite 日志不落盘的部署配置，以及验证。

## 8. 安全

- 本文档和仓库中不保存任何密钥。
- 本项目过程中有 OSS AccessKey 和 GitHub token 以明文出现在聊天记录里，**应当轮换或吊销**。
- 测试数据库里会残留 `codex_fork_test_*` schema，可以定期清理。
