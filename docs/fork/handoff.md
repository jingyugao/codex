# Codex fork 交接文档

更新时间：2026-11-07。仓库 `jingyugao/codex`（上游 `openai/codex`），主干 `main`，当前分支 `feat/fork-v2`。

## 1. 定位与原则

本 fork 只做 codex 内部必须改、又没有上游扩展口的部分，并开放注册口；高可用（PG 存储、跨 pod 交接、沙箱绑定）和多人聊天在独立仓库 [`jingyugao/hcodex`](https://github.com/jingyugao/hcodex)，它依赖本 fork。

本 fork 不合并回上游，但要持续拉取上游更新，所以**对上游文件的改动越少越好**：

- 插桩：每个被改动的上游 crate 有一个 `src/fork_hooks/` 目录，fork 在该 crate 里的代码都放在这里。上游文件只**插入**对 `crate::fork_hooks::xxx(...)` 的单行调用，尽量不修改原有行。`git grep -n "fork_hooks"` 列出所有接入点。
- 需要访问上游私有字段的方法也放在 `fork_hooks/` 目录，在对应上游文件末尾用 `#[path = "fork_hooks/xxx.rs"] mod fork;` 挂成它的子模块（子模块可以访问父模块的私有字段）。`#[path]` 里不要写 `..`（上游 CI 的 `cargo shear` 不认，会报 unlinked file），上游文件在子目录时把 `fork_hooks/` 放在它旁边，例如 `hooks/src/engine/fork_hooks/`。
- 所有接入点都用 `// FORK` 注释标出。
- 不改 `Cargo.toml` / `Cargo.lock`、配置类型、协议定义和生成文件（schema JSON、TypeScript、Python SDK）。
- 上游已有的能力（例如排队消息带 metadata）直接用，不自己实现。

## 2. 分支

- `main`：已重置为上游 `10cf2b9`；`feat/fork-v2` 通过 PR #3 合入。
- `main_backup`：旧 `main` 的备份（早期 PR #1、#2 的实现，已被取代）。
- `feat/fork-v1` / `feat/fork-slots`：旧分支，保留作参考（含已迁到 hcodex 的 `codex-rs/fork/*`）。

同步上游：跟上游 release tag（`rust-v*`）而不是 main，用 `git merge <tag>`，不再 rebase + force push；开启 `git config rerere.enabled true`。合并后跑第 5 节的测试，再在 hcodex 里升级依赖并跑它的测试。

**禁止把编译产物提交进 git。**

## 3. 功能

### 3.1 per-turn env（`fork.env.*`）

客户端在 `turn/start` 的已有字段 `responsesapiClientMetadata` 里传 `fork.env.<NAME>`：

```json
{"method":"turn/start","params":{"threadId":"…","input":[…],
  "responsesapiClientMetadata":{"fork.env.TRACE_ID":"abc123"}}}
```

- 去掉前缀后作为环境变量注入：本地 command hook、远程 environment hook、`exec_command`。
- 这些条目在发给模型 provider 和 MCP server 之前会被剔除。
- 限制：上游对整份 metadata 限制最多 16 条、key 不超过 64 字节、值不超过 128 字节，超出的条目被上游丢弃（`core/src/responses_metadata.rs`）。fork 另外丢弃变量名不匹配 `[A-Za-z_][A-Za-z0-9_]*` 或值含 NUL 的条目。**都是静默丢弃，不返回错误**，客户端要自己按这些规则校验（Colony 的 `packages/chat/src/input.ts` 已经做了）。
- 并入活动 turn 时（`turn/start` 或 `turn/steer`），整份 metadata 被替换，之后启动的 exec 和 hook 使用新 env。
- 跨 pod 恢复的 turn 没有 client metadata，宿主可以通过 `codex_core::fork_hooks::ForkTurnEnv`（turn extension data，按 turn_id 生效）把 env 带过去（hcodex 的交接就是这样做的）。
- 需要客户端开启 `experimentalApi`。

排队消息同样可以带 env：上游的 `thread/queue/add` 已有可选字段 `responsesapiClientMetadata`，语义与 `turn/start` 相同。

```json
{"method":"thread/queue/add","params":{"threadId":"…","input":[…],"clientUserMessageId":"u1:…",
  "responsesapiClientMetadata":{"fork.env.COLONY_USER":"u1"}}}
```

- 上游实现：和输入一起存进队列记录，出队（`thread/queue/start` 或空闲自动派发）时作为该 turn 的 metadata。
- `thread/queue/update` 省略或传空 metadata 时保留原来的；传非空时替换。
- 注意：上游的 `thread/queue/list` 会返回 metadata（包括 `fork.env.*` 的值）。
- env 以明文存在本地 sqlite 队列库中，出队或删除时随记录删除。
- hcodex 的 `queue_env` 测试检查上游这条链路仍然把 metadata 带到 turn env，上游改动排队派发时由它报警。

### 3.2 非本地 thread store 的 resume

`thread/resume` 原本要求 rollout 文件路径，非本地的 thread store（例如 hcodex 的 PG store）没有。fork 把缺失的路径当作空路径继续，响应里不带 `path`。

已知未处理：`thread/revert` 在非本地 store 下仍会因为没有 rollout path 报错，目前没有客户端用到。

### 3.3 注册口（给 hcodex 这类宿主二进制用）

宿主在自己的 `main` 里、启动 app-server 之前调用：

| 函数 | 作用 |
|---|---|
| `codex_core::fork_hooks::set_thread_store_factory(f)` | `f(&Config)` 返回 `Some(store)` 时替换配置的 thread store；只能设一次 |
| `codex_app_server::fork_hooks::add_extension_installer(f)` | 每次构建 thread extension registry 时调用 `f(builder, thread_manager)` 安装扩展 |

本 fork 不依赖任何宿主 crate；不注册时行为与上游一致（per-turn env 除外）。

### 3.4 Code Mode（模型用 JS 一次调用多个工具）

上游已有的功能，fork 不改代码，只负责构建和发布：

- 模型写一段 JS，由 `codex-code-mode-host`（内嵌 V8）执行。JS 里的 `tools.exec_command(...)` 等调用会回到 Codex 按普通工具调用处理，所以命令仍然在 thread 选中的环境里执行，per-turn `fork.env.*` 同样生效。
- 部署：`codex-code-mode-host` 必须和 `codex-app-server`（或 `codex`）放在同一目录，app-server 会在自己旁边找它。
- 是否使用 code mode 首先由模型目录决定：模型的 `tool_mode` 优先于 `[features] code_mode` 开关。目前目录里的模型（如 `gpt-6.1-sol`、`gpt-6-astra`）都是 `code_mode_only`，**部署时必须带上 host**。
- 找不到 host 时：`tool_mode = code_mode` 的模型会退回到直接调用工具；`code_mode_only` 的模型**不会退回**，模型拿不到任何工具，命令全部失败（只有一条 `Code Mode is unavailable … fail closed` 警告）。
- 构建：`scripts/fork/build.sh` 默认构建 host（不需要可加 `--without-code-mode`）。它会从 `github.com/openai/codex/releases` 的 `rusty-v8-v<版本>` 下载与 `Cargo.lock` 版本对应的预编译 V8，校验 sha256 后再设置 `RUSTY_V8_ARCHIVE` / `RUSTY_V8_SRC_BINDING_PATH`。CI 的 release 包里一直带着 host；hcodex 的 `sandbox_e2e` 有 code mode 用例。

## 4. 对上游文件的改动（冲突点）

`git diff main...HEAD --stat -- codex-rs ':!codex-rs/*/fork_hooks/*'` 列出对上游已有文件的全部改动（`main` 重置后即上游）。

插桩目录：

| 目录 | 内容 |
|---|---|
| `core/src/fork_hooks/` | `mod.rs`：core 的接入函数和 `set_thread_store_factory`；`env.rs`：per-turn env；`turn_metadata.rs`：`turn_metadata` 的子模块（读私有 metadata） |
| `hooks/src/fork_hooks/` | `registry.rs`：`Hooks` 的子模块（加 `with_command_env`） |
| `hooks/src/engine/fork_hooks/` | `command_runner.rs`：`CommandHookRuntime` 的子模块（加 `with_command_env`） |
| `app-server/src/fork_hooks/` | `mod.rs`：`add_extension_installer`、resume 时的 rollout path 处理 |

接入点（上游已有文件）：

| 文件 | 改动 |
|---|---|
| `core/src/lib.rs`、`app-server/src/lib.rs` | 声明 `pub mod fork_hooks` |
| `app-server/src/extensions.rs` | `install_extensions`（插入 1 行） |
| `app-server/src/request_processors/thread_processor.rs` | `resume_rollout_path`、`drop_empty_path`（各插入 1 行） |
| `core/src/thread_store_builder.rs` | `thread_store`：宿主注册了 factory 时用它的 store |
| `core/src/unified_exec/process_manager.rs` | `exec_shell_policy`：把 fork env 合进 shell 环境策略的 `set`，本地、远程、shell snapshot 三条路径都用它 |
| `core/src/hook_environment_executor.rs` | `hooks_with_turn_env`（改 1 行）、`environment_hook_env`（插入 1 行） |
| `core/src/turn_metadata.rs` | `strip_client_env`（插入 1 行）：外发 metadata 剔除 `fork.env.*`；末尾挂子模块 |
| `hooks/src/registry.rs`、`hooks/src/engine/command_runner.rs` | 末尾挂子模块 |

最容易出问题的是 `process_manager.rs` 和 `hook_environment_executor.rs`：上游改动频繁，而且可能编译通过但 env 没传到。本仓库的单测只覆盖过滤规则；env 真正到达命令和 hook 由 hcodex 的 e2e 测试（`handoff_e2e`、`sandbox_e2e`、`queue_env`）验证，同步上游后必须在 hcodex 跑一遍。

## 5. 构建与测试

codex-rs 很大：175 个 crate，`codex-core` 单 crate 约 27 万行，debug 版 app-server 约 900 MB。并行度按内存来，**每个 job 约 2–3 GB**。

```sh
cd codex-rs
cargo test -p codex-core --lib fork_hooks
cargo test -p codex-hooks --lib
cargo clippy -p codex-core -p codex-hooks -p codex-app-server --lib -- -D warnings
```

### 构建脚本

`scripts/fork/build.sh`：

```sh
# 全新机器：克隆并构建
curl -fsSL https://raw.githubusercontent.com/jingyugao/codex/feat/fork-v2/scripts/fork/build.sh \
  | bash -s -- --install-deps --ref feat/fork-v2
# 已有源码目录（无需 git）
scripts/fork/build.sh --source . --install-deps
```

- 默认构建 release 版 `codex` 和 `codex-exec-server`。`codex app-server` 是 `codex` 的子命令。
- 可选参数：`--jobs N`、`--profile debug`、`--prefix DIR`、`--with-code-mode`（需要下载 V8）。
- 产物是本机原生动态链接版：依赖构建机的 glibc 版本，以及 `libssl.so.3`。

### CI（`.github/workflows/fork-release.yml`）

- push 到 `feat/fork-**` 分支时触发：
  - `build`：musl 静态 release 包（`codex`、`codex-code-mode-host`、`codex-exec-server`），产物作为 artifact 上传。
  - `fork-tests`：上面的单测与 clippy。
- 发布 GitHub Release 只在手动触发时进行：`workflow_dispatch` 且 `publish=true`。
- 首次运行没有缓存，或 `Cargo.lock` 变化后，需要 1 小时以上。

## 6. 发布流程（二进制）

1. 远程机器从 git 拉取 `feat/fork-v2`，用构建脚本构建。
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

## 7. 后续

- [ ] PR #3 的 CI 通过后合并。
- [ ] 发布包精简：只发 `codex`（`codex app-server` / `codex exec-server` 都是子命令）和 `codex-code-mode-host`，release tag 构建开 LTO（hcodex 发布后，本仓库的发布包可能不再需要）。

## 8. 安全

- 本文档和仓库中不保存任何密钥。
- 本项目过程中有 OSS AccessKey 和 GitHub token 以明文出现在聊天记录里，**应当轮换或吊销**。
