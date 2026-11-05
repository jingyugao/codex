// Deterministic end-to-end verification against the real forked App Server.
// Uses a controlled Responses endpoint; shell effects are real, model inference is not.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';
import { mkdtemp, mkdir, readFile, writeFile, access } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';

const root = await mkdtemp(join(tmpdir(), 'codex-turn-controls-'));
for (const name of ['home', 'codex', 'workspace']) await mkdir(join(root, name));
const workspace = join(root, 'workspace');
const catalog = join(root, 'models.json');
await writeFile(catalog, JSON.stringify({ models: [{
  slug: 'fixture-model', display_name: 'Fixture', description: 'Controlled runtime verification',
  default_reasoning_level: 'medium', supported_reasoning_levels: [], shell_type: 'unified_exec',
  visibility: 'list', supported_in_api: true, priority: 0, availability_nux: null, upgrade: null,
  experimental_supported_tools: ['send_user_message_async'], tool_mode: 'direct', input_modalities: ['text'],
  support_verbosity: false, default_verbosity: null, apply_patch_tool_type: 'freeform',
  web_search_tool_type: 'text_and_image', truncation_policy: { mode: 'tokens', limit: 10000 },
  context_window: 128000, effective_context_window_percent: 95, supports_reasoning_summary_parameter: false,
  base_instructions: 'You are an isolated runtime verification fixture.',
}] }));
let handler;
let responses = 0;
const requests = [];
const stream = (response, type, data) => response.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`);
function complete(response, items) {
  const id = `response_${++responses}`;
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  stream(response, 'response.created', { response: { id } });
  items.forEach((item, index) => {
    stream(response, 'response.output_item.added', { output_index: index, item });
    stream(response, 'response.output_item.done', { output_index: index, item });
  });
  stream(response, 'response.completed', { response: { id, status: 'completed', output: items,
    usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } });
  response.end();
}
const final = response => complete(response, [{ type: 'message', id: `msg_${responses}`,
  role: 'assistant', content: [{ type: 'output_text', text: 'FIXTURE_DONE' }] }]);
function execute(response, body, cmd, callId) {
  const tools = body.tools.flatMap(tool => tool.type === 'namespace'
    ? tool.tools.map(child => ({ ...child, namespace: tool.name })) : [tool]);
  const tool = tools.find(tool => tool.name === 'exec_command');
  assert(tool, 'Model request did not expose exec_command');
  complete(response, [{ type: 'function_call', id: `fc_${callId}`, call_id: callId,
    name: tool.name, ...(tool.namespace ? { namespace: tool.namespace } : {}),
    arguments: JSON.stringify({ cmd, workdir: workspace, login: false, yield_time_ms: 1000 }) }]);
}
const server = createServer(async (request, response) => {
  try {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks));
    if (body.generate === false) { final(response); return; }
    requests.push(body);
    await handler(body, response);
  } catch (error) {
    console.error('FIXTURE_SERVER_ERROR', error.message);
    response.writeHead(500).end();
  }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
// Configure trusted local command hooks. Log only synthetic fixture variables.
const hookLog = join(workspace, 'hook-env.log');
const asyncReady = join(workspace, 'async-hook-ready');
const asyncRelease = join(workspace, 'async-hook-release');
const asyncResult = join(workspace, 'async-hook-result');
const hookNames = ['UserPromptSubmit', 'PreToolUse', 'PostToolUse', 'Stop', 'Interrupt'];
const hookLogger = join(root, 'hook-logger.cjs');
await writeFile(hookLogger, `const fs = require('node:fs');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
fs.appendFileSync(${JSON.stringify(hookLog)}, [input.turn_id, process.argv[2], process.env.TRACE_MARKER ?? 'unset', process.env.HOOK_BASE_MARKER].join('|') + '\\n');
`);
const configLines = ['[features]', 'hooks = true'];
for (const name of hookNames) {
  const command = `${JSON.stringify(process.execPath)} ${JSON.stringify(hookLogger)} ${name}`;
  configLines.push(`[[hooks.${name}]]`, `[[hooks.${name}.hooks]]`, 'type = "command"',
    `command = ${JSON.stringify(command)}`);
}
// Hold a background hook until steer updates the active turn; its process must retain its original environment.
const asyncCommand = `if [ "${"${TRACE_MARKER-unset}"}" = TRACE_A ]; then : > ${JSON.stringify(asyncReady)}; i=0; while [ ! -f ${JSON.stringify(asyncRelease)} ] && [ "$i" -lt 200 ]; do i=$((i + 1)); sleep 0.05; done; printf '%s' "$TRACE_MARKER" > ${JSON.stringify(asyncResult)}; fi`;
configLines.push('[[hooks.UserPromptSubmit]]', '[[hooks.UserPromptSubmit.hooks]]', 'type = "command"',
  `command = ${JSON.stringify(asyncCommand)}`, 'async = true');
await writeFile(join(root, 'codex', 'config.toml'), configLines.join('\n'));
const overrides = { 'features.hooks': true, model: 'fixture-model', model_catalog_json: catalog, model_provider: 'local_fixture',
  'features.enable_request_compression': false, 'features.apps': false, 'features.memories': false,
  'features.shell_snapshot': false,
  'model_providers.local_fixture.name': 'Local verification fixture',
  'model_providers.local_fixture.base_url': `http://127.0.0.1:${server.address().port}/v1`,
  'model_providers.local_fixture.wire_api': 'responses',
  'model_providers.local_fixture.requires_openai_auth': false };
const binary = process.env.CODEX_FORK_BINARY || resolve(import.meta.dirname, '../codex-rs/target/debug/codex-app-server');
const child = spawn(binary, Object.entries(overrides).flatMap(([key, value]) => ['-c', `${key}=${JSON.stringify(value)}`]), {
  cwd: workspace, env: { PATH: process.env.PATH, HOOK_BASE_MARKER: 'base-hook', HOME: join(root, 'home'), CODEX_HOME: join(root, 'codex'), TMPDIR: root },
  stdio: ['pipe', 'pipe', 'pipe'],
});
let stderr = '';
child.stderr.on('data', chunk => { stderr += chunk.toString(); });
const pending = new Map();
const events = [];
let sequence = 0;
const lines = createInterface({ input: child.stdout });
lines.on('line', line => {
  const value = JSON.parse(line);
  if (value.id !== undefined && pending.has(value.id)) {
    const { resolve, reject, timer } = pending.get(value.id); pending.delete(value.id); clearTimeout(timer);
    if (value.error) reject(Object.assign(new Error(value.error.message), { code: value.error.code }));
    else resolve(value.result);
  } else if (value.method && value.id === undefined) events.push(value);
});
function rpc(method, params) {
  const id = ++sequence;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`Timed out: ${method}`)); }, 20_000);
    pending.set(id, { resolve, reject, timer });
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
  });
}
async function until(predicate) {
  const deadline = Date.now() + 20_000;
  while (!(await predicate())) {
    if (Date.now() > deadline) throw new Error('Verification condition timed out');
    if (child.exitCode !== null) throw new Error(`App Server exited: ${child.exitCode}`);
    await delay(10);
  }
}
const exists = path => access(join(workspace, path)).then(() => true, () => false);
const event = (method, turnId) => events.find(item => item.method === method
  && (item.params.turnId === turnId || item.params.turn?.id === turnId));
const ended = async (turnId, status = 'completed') => {
  await until(() => event('turn/completed', turnId));
  const result = event('turn/completed', turnId);
  assert.equal(result.params.turn.status, status);
  return result;
};
const thread = async () => (await rpc('thread/start', { cwd: workspace, model: 'fixture-model',
  approvalPolicy: 'never', sandbox: 'danger-full-access' })).thread.id;
const start = async (threadId, text, env) => (await rpc('turn/start', { threadId,
  input: [{ type: 'text', text }], ...(env === undefined ? {} : { env }) })).turn.id;
const report = { binary, root, realModelCalls: 0, cases: [] };
async function check(name, action) {
  console.log(`START ${name}`);
  const started = Date.now();
  await action();
  report.cases.push({ name, status: 'passed', durationMs: Date.now() - started });
  console.log(`PASS ${name}`);
}
try {
  await rpc('initialize', { clientInfo: { name: 'fork_verification', version: '1' }, capabilities: { experimentalApi: true } });
  child.stdin.write(JSON.stringify({ method: 'initialized', params: {} }) + '\n');
  const listed = await rpc('hooks/list', { cwds: [workspace] });
  const hookState = Object.fromEntries(listed.data[0].hooks.map(hook => [hook.key, { trusted_hash: hook.currentHash }]));
  assert(Object.keys(hookState).length >= hookNames.length, 'Expected command hooks to be discovered');
  await rpc('config/batchWrite', { edits: [{ keyPath: 'hooks.state', value: hookState, mergeStrategy: 'upsert' }], reloadUserConfig: true });

  await check('env-start-steer-snapshot-and-new-turn-reset', async () => {
    let firstResponse;
    let count = 0;
    handler = async (body, response) => {
      count++;
      if (count === 1) firstResponse = { body, response };
      else if (count === 2) execute(response, body, `printf '%s' "$TRACE_MARKER" > second.trace`, 'env_second');
      else final(response);
    };
    const id = await thread();
    const turnId = await start(id, 'ENV_SNAPSHOT_TASK', { TRACE_MARKER: 'TRACE_A' });
    await until(() => firstResponse);
    await until(() => exists('async-hook-ready'));
    await assert.rejects(rpc('turn/steer', { threadId: id, expectedTurnId: 'stale',
      input: [{ type: 'text', text: 'rejected' }], env: { TRACE_MARKER: 'WRONG' } }));
    execute(firstResponse.response, firstResponse.body,
      'printf started > env-running.flag; sleep 0.5; printf \'%s\' "$TRACE_MARKER" > first.trace', 'env_first');
    await until(() => exists('env-running.flag'));
    await rpc('turn/steer', { threadId: id, expectedTurnId: turnId,
      input: [{ type: 'text', text: 'Use the updated trace for the next step.' }], env: { TRACE_MARKER: 'TRACE_B' } });
    await writeFile(asyncRelease, 'release');
    await until(() => exists('async-hook-result'));
    assert.equal(await readFile(asyncResult, 'utf8'), 'TRACE_A');
    await ended(turnId);
    assert.equal(await readFile(join(workspace, 'first.trace'), 'utf8'), 'TRACE_A');
    assert.equal(await readFile(join(workspace, 'second.trace'), 'utf8'), 'TRACE_B');
    assert(!JSON.stringify(requests).includes('TRACE_A'), 'Environment values must stay out of model input');
    assert(!JSON.stringify(requests).includes('TRACE_B'), 'Steered environment values must stay out of model input');
    let resetCount = 0;
    handler = async (body, response) => {
      if (++resetCount === 1) execute(response, body, 'printf \'%s\' "${TRACE_MARKER-unset}" > reset.trace', 'env_reset');
      else final(response);
    };
    const resetTurn = await start(id, 'NEW_TURN_WITHOUT_ENV');
    await ended(resetTurn);
    assert.equal(await readFile(join(workspace, 'reset.trace'), 'utf8'), 'unset');
    const logs = (await readFile(hookLog, 'utf8')).trim().split('\n');
    for (const [hookTurn, name, trace] of [[turnId, 'UserPromptSubmit', 'TRACE_A'], [turnId, 'PreToolUse', 'TRACE_A'],
      [turnId, 'PostToolUse', 'TRACE_A'], [turnId, 'PreToolUse', 'TRACE_B'], [turnId, 'PostToolUse', 'TRACE_B'],
      [turnId, 'Stop', 'TRACE_B'], [resetTurn, 'UserPromptSubmit', 'unset'], [resetTurn, 'Stop', 'unset']]) {
      assert(logs.includes(`${hookTurn}|${name}|${trace}|base-hook`), `Missing hook snapshot ${name}/${trace}`);
    }
  });

  await check('concurrent-threads-keep-environments-isolated', async () => {
    const counts = new Map();
    handler = async (body, response) => {
      const marker = JSON.stringify(body.input).includes('CONCURRENT_ENV_A') ? 'A' : 'B';
      const count = (counts.get(marker) || 0) + 1;
      counts.set(marker, count);
      if (count === 1) execute(response, body,
        `sleep 0.2; printf '%s' "$TRACE_MARKER" > concurrent-${marker}.trace`, `concurrent_${marker}`);
      else final(response);
    };
    const [threadA, threadB] = await Promise.all([thread(), thread()]);
    const turns = await Promise.all([
      start(threadA, 'CONCURRENT_ENV_A', { TRACE_MARKER: 'ONLY_A' }),
      start(threadB, 'CONCURRENT_ENV_B', { TRACE_MARKER: 'ONLY_B' }),
    ]);
    await Promise.all(turns.map(turnId => ended(turnId)));
    assert.equal(await readFile(join(workspace, 'concurrent-A.trace'), 'utf8'), 'ONLY_A');
    assert.equal(await readFile(join(workspace, 'concurrent-B.trace'), 'utf8'), 'ONLY_B');
    const logs = (await readFile(hookLog, 'utf8')).trim().split('\n');
    for (const [hookTurn, trace] of [[turns[0], 'ONLY_A'], [turns[1], 'ONLY_B']]) for (const name of ['UserPromptSubmit', 'PreToolUse', 'PostToolUse', 'Stop']) {
      assert(logs.includes(`${hookTurn}|${name}|${trace}|base-hook`), `Missing concurrent hook ${name}/${trace}`);
    }
  });

  await check('queued-env-auto-dispatch-and-update', async () => {
    let held;
    let count = 0;
    handler = async (body, response) => {
      const n = ++count;
      if (n === 1) held = { body, response };
      else if (n % 2 === 1) execute(response, body,
        `printf '%s' "${'${TRACE_MARKER-unset}'}" > queue-${(n - 1) / 2}.trace`, `queue_${n}`);
      else final(response);
    };
    const id = await thread();
    const turnId = await start(id, 'QUEUE_ACTIVE', { TRACE_MARKER: 'ACTIVE_TRACE' });
    await until(() => held);
    const add = (text, env) => rpc('thread/queue/add', { threadId: id,
      input: [{ type: 'text', text }], clientUserMessageId: text,
      ...(env === undefined ? {} : { env }) });
    const first = await add('QUEUED_FIRST', { TRACE_MARKER: 'QUEUE_FIRST' });
    await add('QUEUED_SECOND', { TRACE_MARKER: 'QUEUE_SECOND' });
    await add('QUEUED_EMPTY');
    const before = await rpc('thread/queue/list', { threadId: id });
    assert.equal(before.data.length, 3);
    await assert.rejects(add('QUEUED_INVALID', { 'INVALID=KEY': 'bad' }));
    const after = await rpc('thread/queue/list', { threadId: id });
    assert.deepEqual(after.data, before.data, 'Invalid env changed the queue');
    await rpc('thread/queue/update', { threadId: id, queuedSubmissionId: first.queuedSubmission.id,
      input: [{ type: 'text', text: 'QUEUED_FIRST_UPDATED' }] });
    execute(held.response, held.body, 'printf \'%s\' "$TRACE_MARKER" > queue-active.trace', 'queue_active');
    await ended(turnId);
    await until(() => events.filter(item => item.method === 'turn/completed' && item.params.threadId === id).length === 4);
    assert.equal(await readFile(join(workspace, 'queue-active.trace'), 'utf8'), 'ACTIVE_TRACE');
    for (const [index, value] of [[1, 'QUEUE_FIRST'], [2, 'QUEUE_SECOND'], [3, 'unset']]) {
      assert.equal(await readFile(join(workspace, `queue-${index}.trace`), 'utf8'), value);
    }
    const completed = events.filter(item => item.method === 'turn/completed' && item.params.threadId === id);
    const logs = (await readFile(hookLog, 'utf8')).trim().split('\n');
    for (const [index, trace] of [[0, 'ACTIVE_TRACE'], [1, 'QUEUE_FIRST'], [2, 'QUEUE_SECOND'], [3, 'unset']]) {
      assert.equal(completed[index].params.turn.status, 'completed');
      for (const name of ['UserPromptSubmit', 'PreToolUse', 'PostToolUse', 'Stop']) {
        assert(logs.includes(`${completed[index].params.turn.id}|${name}|${trace}|base-hook`), `Missing queued hook ${name}/${trace}`);
      }
    }
    assert(JSON.stringify(requests).includes('QUEUED_FIRST_UPDATED'));
    for (const trace of ['ACTIVE_TRACE', 'QUEUE_FIRST', 'QUEUE_SECOND']) {
      assert(!JSON.stringify(requests).includes(trace), 'Queued environment leaked into model input');
    }
    assert.equal((await rpc('thread/queue/list', { threadId: id })).data.length, 0);
  });

  await check('queued-env-explicit-start-after-interrupt', async () => {
    let held;
    let count = 0;
    handler = async (body, response) => {
      const n = ++count;
      if (n === 1) held = { body, response };
      else if (n % 2 === 0) execute(response, body,
        `printf '%s' "${'${TRACE_MARKER-unset}'}" > queue-explicit-${n / 2}.trace`, `queue_explicit_${n}`);
      else final(response);
    };
    const id = await thread();
    const turnId = await start(id, 'QUEUE_INTERRUPT', { TRACE_MARKER: 'INTERRUPTED_TRACE' });
    await until(() => held);
    const add = (text, trace) => rpc('thread/queue/add', { threadId: id,
      input: [{ type: 'text', text }], clientUserMessageId: text, env: { TRACE_MARKER: trace } });
    await add('EXPLICIT_LATER', 'EXPLICIT_LATER_TRACE');
    const chosen = await add('EXPLICIT_FIRST', 'EXPLICIT_FIRST_TRACE');
    await rpc('turn/interrupt', { threadId: id, turnId });
    await ended(turnId, 'interrupted');
    const started = await rpc('thread/queue/start', { threadId: id, queuedSubmissionId: chosen.queuedSubmission.id });
    await ended(started.turn.id);
    await until(() => events.filter(item => item.method === 'turn/completed' && item.params.threadId === id).length === 3);
    assert.equal(await readFile(join(workspace, 'queue-explicit-1.trace'), 'utf8'), 'EXPLICIT_FIRST_TRACE');
    assert.equal(await readFile(join(workspace, 'queue-explicit-2.trace'), 'utf8'), 'EXPLICIT_LATER_TRACE');
    const completed = events.filter(item => item.method === 'turn/completed' && item.params.threadId === id);
    const logs = (await readFile(hookLog, 'utf8')).trim().split('\n');
    for (const [index, trace] of [[1, 'EXPLICIT_FIRST_TRACE'], [2, 'EXPLICIT_LATER_TRACE']]) {
      assert.equal(completed[index].params.turn.status, 'completed');
      for (const name of ['UserPromptSubmit', 'PreToolUse', 'PostToolUse', 'Stop']) {
        assert(logs.includes(`${completed[index].params.turn.id}|${name}|${trace}|base-hook`), `Missing explicit queue hook ${name}/${trace}`);
      }
    }
    assert.equal((await rpc('thread/queue/list', { threadId: id })).data.length, 0);
    held.response.destroy();
  });

  await check('ordinary-interrupt-resume-without-user-message', async () => {
    let held;
    let resumedRequest;
    let count = 0;
    const initialRequest = requests.length;
    handler = async (body, response) => {
      if (++count === 1) held = { body, response };
      else if (count === 2) resumedRequest = { body, response };
      else final(response);
    };
    const id = await thread();
    const turnId = await start(id, 'ORIGINAL_INTERRUPTED_TASK', { TRACE_MARKER: 'RESUMED_TRACE' });
    await until(() => held);
    await assert.rejects(rpc('turn/resume', { threadId: id, turnId }));
    await rpc('turn/interrupt', { threadId: id, turnId });
    await ended(turnId, 'interrupted');
    const interruptLogs = (await readFile(hookLog, 'utf8')).trim().split('\n');
    assert(interruptLogs.includes(`${turnId}|Interrupt|RESUMED_TRACE|base-hook`));
    const resumed = await rpc('turn/resume', { threadId: id, turnId });
    assert.notEqual(resumed.turnId, turnId);
    await until(() => resumedRequest);
    await assert.rejects(rpc('turn/resume', { threadId: id, turnId }));
    execute(resumedRequest.response, resumedRequest.body,
      'printf \'%s\' "$TRACE_MARKER" > resumed.trace', 'resumed_env');
    await ended(resumed.turnId);
    assert.equal(await readFile(join(workspace, 'resumed.trace'), 'utf8'), 'RESUMED_TRACE');
    const userText = (requests.at(-1).input || []).filter(item => item.role === 'user')
      .map(item => JSON.stringify(item.content)).join('\n');
    assert.equal(userText.split('ORIGINAL_INTERRUPTED_TASK').length - 1, 1);
    assert(!userText.includes('继续'));
    assert.equal(requests.length - initialRequest, 3);
    assert.equal(event('turn/resumed', resumed.turnId), undefined);
    await assert.rejects(rpc('turn/resume', { threadId: id, turnId }));
    held.response.destroy();
  });

  report.status = 'passed';
} catch (error) {
  report.status = 'failed'; report.error = { message: error.message, stack: error.stack };
  console.error(error.message); process.exitCode = 1;
} finally {
  for (const { reject, timer } of pending.values()) { clearTimeout(timer); reject(new Error('Verification ended')); }
  child.stdin.end(); child.kill('SIGTERM');
  server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
  await writeFile(join(root, 'report.json'), JSON.stringify(report, null, 2));
  await writeFile(join(root, 'server.stderr.log'), stderr);
  await writeFile(join(root, 'events.json'), JSON.stringify(events, null, 2));
  console.log(JSON.stringify({ status: report.status, cases: report.cases.length, report: join(root, 'report.json') }));
}
