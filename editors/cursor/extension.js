'use strict';

/**
 * jev for Cursor.
 *
 * Cursor has no setting that points it at an arbitrary stdio language server — its own docs
 * list MCP servers and plugin paths under `vscode.cursor` and nothing else, and `mcp.json` is
 * a different protocol — so the supported route is an extension that starts one. This is that
 * extension, and it is the whole client: no build step, no `npm install`, no
 * `vscode-languageclient`. `child_process` plus the same `Content-Length` JSON-RPC framing the
 * server already speaks (PROTOCOL.md §2).
 *
 * It exposes exactly what the server serves and nothing more: pull diagnostics, code actions
 * with `resolve`, code lenses, hover, inlay hints and `workspace/executeCommand`, plus the
 * server->client half — `workspace/applyEdit`, `workspace/configuration`,
 * `window/showDocument`, `window/workDoneProgress/create` and the three refresh
 * notifications. `inlineCompletionProvider` is not advertised by the server, so nothing here
 * renders one.
 *
 * Two details are load-bearing, and both are in `docs/CURSOR.md`:
 *
 * 1. The server pins `positionEncoding` to `"utf-8"` (PROTOCOL.md §2) and its `initialize`
 *    never reads the client's list, so every `character` on the wire is a byte offset into the
 *    line while the VS Code API counts UTF-16 code units. `utf8ToUtf16`/`utf16ToUtf8` are that
 *    crossing; without them every range after the first non-ASCII character lands wrong.
 * 2. The server reads its endpoints from *its own environment* (§10), and a macOS GUI app does
 *    not inherit the login shell's environment. So the endpoints travel as Cursor settings and
 *    are applied to the child process, and the decide tier's key is read from a file the user
 *    already has rather than pasted into a setting. The value is never logged.
 */

const cp = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const vscode = require('vscode');

const CLIENT_NAME = 'jev-cursor';
const CLIENT_VERSION = '0.1.0';
const DIAGNOSTIC_SOURCE = 'jev';
const DEFAULT_DECIDE_KEY_VARIABLE = 'TYPESAFE_API_KEY';

/** The code action kinds the server advertises (PROTOCOL.md §2), so VS Code can filter. */
const CODE_ACTION_KINDS = [
  'quickfix',
  'quickfix.jev',
  'refactor.rewrite',
  'refactor.rewrite.jev',
  'source',
  'source.jev',
  'source.fixAll',
].map((kind) => new vscode.CodeActionKind(kind));

/** A file document, the only kind that is attached (`docs/LANGUAGE.md` §1). */
function isAnalysable(document) {
  return document !== undefined && document.uri.scheme === 'file';
}

// ---------------------------------------------------------------------------
// utf-8 <-> utf-16
// ---------------------------------------------------------------------------

/** A wire byte offset -> a UTF-16 offset into `lineText`, snapped back to a code point. */
function utf8ToUtf16(lineText, byteOffset) {
  if (!(byteOffset > 0)) return 0;
  const bytes = Buffer.from(lineText, 'utf8');
  if (byteOffset >= bytes.length) return lineText.length;
  let end = byteOffset;
  // `(b & 0xc0) === 0x80` is a continuation byte: the offset landed inside a code point, so
  // step back to that code point's first byte rather than decoding half of one.
  while (end > 0 && (bytes[end] & 0xc0) === 0x80) end -= 1;
  return bytes.toString('utf8', 0, end).length;
}

/** A UTF-16 offset into `lineText` -> a wire byte offset. */
function utf16ToUtf8(lineText, unitOffset) {
  if (!(unitOffset > 0)) return 0;
  if (unitOffset >= lineText.length) return Buffer.byteLength(lineText, 'utf8');
  let end = unitOffset;
  const unit = lineText.charCodeAt(end);
  // A low surrogate is the second half of a pair: step back so the prefix is whole.
  if (unit >= 0xdc00 && unit <= 0xdfff) end -= 1;
  return Buffer.byteLength(lineText.slice(0, end), 'utf8');
}

function lineTextAt(document, line) {
  if (line < 0 || line >= document.lineCount) return '';
  return document.lineAt(line).text;
}

/** Wire position -> `vscode.Position`. */
function toVscodePosition(document, position) {
  const last = Math.max(0, document.lineCount - 1);
  const line = Math.max(0, Math.min(position.line, last));
  return new vscode.Position(line, utf8ToUtf16(lineTextAt(document, line), position.character));
}

/** `vscode.Position` -> wire position. */
function toWirePosition(document, position) {
  return {
    line: position.line,
    character: utf16ToUtf8(lineTextAt(document, position.line), position.character),
  };
}

function toVscodeRange(document, range) {
  return new vscode.Range(
    toVscodePosition(document, range.start),
    toVscodePosition(document, range.end),
  );
}

function toWireRange(document, range) {
  return {
    start: toWirePosition(document, range.start),
    end: toWirePosition(document, range.end),
  };
}

// ---------------------------------------------------------------------------
// the document the server holds
// ---------------------------------------------------------------------------

/**
 * The text the server has, kept in step with the editor's.
 *
 * `textDocumentSync.change` is `2` (incremental), and an incremental change's `range` is a
 * UTF-8 byte range **against the text before that change** — so the conversion needs the
 * previous line, not the current one. Holding the lines here is what makes that possible, and
 * it is what an LSP client's document manager is for. `didChange` resends the whole text
 * whenever this model and `document.lineCount` disagree, so a dropped event costs one
 * full-text send and never a silently diverged server.
 */
class TextModel {
  constructor(text) {
    this.lines = text.split('\n');
  }

  line(lineNumber) {
    return lineNumber >= 0 && lineNumber < this.lines.length ? this.lines[lineNumber] : '';
  }

  apply(change) {
    const startLine = change.range ? change.range.start.line : 0;
    const endLine = change.range ? change.range.end.line : this.lines.length - 1;
    const startChar = change.range
      ? utf8ToUtf16(this.line(startLine), change.range.start.character)
      : 0;
    const endChar = change.range
      ? utf8ToUtf16(this.line(endLine), change.range.end.character)
      : this.line(endLine).length;
    const inserted = change.text.split('\n');
    inserted[0] = this.line(startLine).slice(0, startChar) + inserted[0];
    const last = inserted.length - 1;
    inserted[last] += this.line(endLine).slice(endChar);
    this.lines.splice(startLine, endLine - startLine + 1, ...inserted);
  }
}

// ---------------------------------------------------------------------------
// the wire shapes this client translates
// ---------------------------------------------------------------------------

const SEVERITY_FROM_WIRE = {
  1: vscode.DiagnosticSeverity.Error,
  2: vscode.DiagnosticSeverity.Warning,
  3: vscode.DiagnosticSeverity.Information,
  4: vscode.DiagnosticSeverity.Hint,
};

const SEVERITY_TO_WIRE = new Map([
  [vscode.DiagnosticSeverity.Error, 1],
  [vscode.DiagnosticSeverity.Warning, 2],
  [vscode.DiagnosticSeverity.Information, 3],
  [vscode.DiagnosticSeverity.Hint, 4],
]);

function toVscodeDiagnostic(document, diagnostic) {
  const out = new vscode.Diagnostic(
    toVscodeRange(document, diagnostic.range),
    diagnostic.message,
    SEVERITY_FROM_WIRE[diagnostic.severity] ?? vscode.DiagnosticSeverity.Warning,
  );
  out.code = diagnostic.code;
  out.source = diagnostic.source ?? DIAGNOSTIC_SOURCE;
  return out;
}

function toWireDiagnostic(document, diagnostic) {
  const code = typeof diagnostic.code === 'object' && diagnostic.code !== null
    ? diagnostic.code.value
    : diagnostic.code;
  return {
    range: toWireRange(document, diagnostic.range),
    severity: SEVERITY_TO_WIRE.get(diagnostic.severity) ?? 2,
    code: code === undefined ? undefined : code,
    source: diagnostic.source,
    message: diagnostic.message,
  };
}

/** Thrown when the editor cannot express an edit the server sent; the caller reports why. */
class EditUnsupported extends Error {}

/**
 * Wire `WorkspaceEdit` -> `vscode.WorkspaceEdit`.
 *
 * PROTOCOL.md §8 rule 1 is "`documentChanges` form only, the `changes` map is rejected", so a
 * `changes` map here is a protocol violation rather than something to guess at. Rule 3 is the
 * client's half and is enforced below: an edit stamped with a version the buffer has moved
 * past is refused, because its ranges were computed against text that is gone.
 */
function toWorkspaceEdit(edit) {
  const out = new vscode.WorkspaceEdit();
  const operations = edit && edit.documentChanges;
  if (!Array.isArray(operations)) {
    throw new EditUnsupported('the edit carried no `documentChanges` (PROTOCOL §8 rule 1)');
  }
  for (const operation of operations) {
    if (operation.kind === 'create') {
      out.createFile(vscode.Uri.parse(operation.uri), { overwrite: false, ignoreIfExists: true });
      continue;
    }
    if (operation.kind === 'rename') {
      out.renameFile(vscode.Uri.parse(operation.oldUri), vscode.Uri.parse(operation.newUri), {
        overwrite: false,
        ignoreIfExists: true,
      });
      continue;
    }
    if (operation.kind === 'delete') {
      out.deleteFile(vscode.Uri.parse(operation.uri), {
        recursive: false,
        ignoreIfNotExists: true,
      });
      continue;
    }
    // TextDocumentEdit: `{textDocument: {uri, version}, edits: [{range, newText}]}`, with the
    // ranges as UTF-8 byte offsets — the server computes them (§8 rule 4), this client only
    // translates them.
    const uri = operation.textDocument.uri;
    const document = vscode.workspace.textDocuments.find((d) => d.uri.toString() === uri);
    if (document === undefined) {
      throw new EditUnsupported(`no open editor for ${uri}, so its ranges cannot be translated`);
    }
    const version = operation.textDocument.version;
    if (version !== null && version !== undefined && version !== document.version) {
      throw new EditUnsupported(
        `the edit is stamped for version ${version} and the buffer is at ${document.version}`,
      );
    }
    const edits = operation.edits.map(
      (e) => new vscode.TextEdit(toVscodeRange(document, e.range), e.newText),
    );
    out.set(document.uri, (out.get(document.uri) ?? []).concat(edits));
  }
  return out;
}

function toVscodeCommand(command) {
  return {
    command: command.command,
    title: command.title,
    arguments: command.arguments,
  };
}

function markdownOf(contents) {
  if (typeof contents === 'string') return contents;
  if (Array.isArray(contents)) return contents.map(markdownOf).join('\n\n');
  if (contents && typeof contents.value === 'string') return contents.value;
  return '';
}

function kindOf(kind) {
  return kind === undefined || kind === ''
    ? vscode.CodeActionKind.Empty
    : new vscode.CodeActionKind(kind);
}

/**
 * The target a `jev.plugin.*` lens command was invoked with.
 *
 * `code_lens` in `crates/jev-lsp/src/server.rs` sends `arguments: [{uri, line}]` — **one
 * object** — and VS Code spreads a command's `arguments` into the handler, so the handler's
 * first parameter is that object. A client that assumes `(uri, line)` binds the object to
 * `uri`, compares it against a string, finds nothing, and the click is a silent no-op; that is
 * how this extension first behaved. The array case is accepted too, and anything else is
 * logged rather than dropped, because a dropped click is the failure this function exists to
 * prevent.
 */
function lensTarget(args) {
  const first = Array.isArray(args[0]) ? args[0][0] : args[0];
  if (first === null || typeof first !== 'object') return undefined;
  if (typeof first.uri !== 'string' || typeof first.line !== 'number') return undefined;
  return { uri: first.uri, line: first.line };
}

// ---------------------------------------------------------------------------
// JSON-RPC over the server's stdio
// ---------------------------------------------------------------------------

class Connection {
  constructor(options) {
    this.log = options.log;
    this.onExit = options.onExit ?? (() => {});
    this.failure = options.failure;
    this.nextId = 1;
    this.pending = new Map();
    this.requests = new Map();
    this.notifications = new Map();
    this.buffer = Buffer.alloc(0);
    this.closed = false;

    this.child = cp.spawn(options.command, options.args, {
      cwd: options.cwd,
      env: options.env,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    this.child.stdout.on('data', (chunk) => this.feed(chunk));
    this.child.stderr.on('data', (chunk) => this.log(String(chunk).replace(/\s+$/, '')));
    this.child.on('error', (error) => {
      this.closed = true;
      this.rejectAll(new Error(`cannot start ${options.command}: ${error.message}`));
      this.failure(error.message);
    });
    this.child.on('exit', (code, signal) => {
      this.closed = true;
      this.rejectAll(new Error(`jev-lsp exited (${signal ?? code})`));
      this.onExit(code, signal);
    });
    // A server that fails after `initialize` (a dead endpoint, a crash) must not take the
    // extension host down with it.
    this.child.stdin.on('error', (error) => this.log(`stdin: ${error.message}`));
  }

  onRequest(method, handler) {
    this.requests.set(method, handler);
  }

  onNotification(method, handler) {
    this.notifications.set(method, handler);
  }

  send(message) {
    if (this.closed || this.child.stdin.destroyed) return;
    const body = Buffer.from(JSON.stringify(message), 'utf8');
    this.child.stdin.write(
      Buffer.concat([Buffer.from(`Content-Length: ${body.length}\r\n\r\n`, 'ascii'), body]),
    );
  }

  request(method, params, token) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      if (this.closed) {
        reject(new Error(`jev-lsp is not running, so ${method} cannot be sent`));
        return;
      }
      const entry = { resolve, reject, cancellation: undefined };
      if (token !== undefined && token !== null) {
        entry.cancellation = token.onCancellationRequested(() => {
          if (this.pending.has(id)) {
            this.send({ jsonrpc: '2.0', method: '$/cancelRequest', params: { id } });
          }
        });
      }
      this.pending.set(id, entry);
      this.send({ jsonrpc: '2.0', id, method, params });
    });
  }

  notify(method, params) {
    this.send({ jsonrpc: '2.0', method, params });
  }

  resolve(id, result) {
    this.send({ jsonrpc: '2.0', id, result: result === undefined ? null : result });
  }

  resolveError(id, code, message) {
    this.send({ jsonrpc: '2.0', id, error: { code, message } });
  }

  settle(id) {
    const entry = this.pending.get(id);
    if (entry === undefined) return undefined;
    this.pending.delete(id);
    entry.cancellation?.dispose();
    return entry;
  }

  rejectAll(error) {
    for (const entry of this.pending.values()) {
      entry.cancellation?.dispose();
      entry.reject(error);
    }
    this.pending.clear();
  }

  feed(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    for (;;) {
      const headerEnd = this.buffer.indexOf('\r\n\r\n');
      if (headerEnd < 0) return;
      const header = this.buffer.toString('ascii', 0, headerEnd);
      const length = /content-length:\s*(\d+)/i.exec(header);
      const start = headerEnd + 4;
      if (length === null) {
        this.log(`discarding a frame with no Content-Length: ${header.slice(0, 120)}`);
        this.buffer = this.buffer.subarray(start);
        continue;
      }
      const size = Number.parseInt(length[1], 10);
      if (this.buffer.length < start + size) return;
      const body = this.buffer.toString('utf8', start, start + size);
      this.buffer = this.buffer.subarray(start + size);
      let message;
      try {
        message = JSON.parse(body);
      } catch {
        this.log(`discarding an unparsable frame: ${body.slice(0, 200)}`);
        continue;
      }
      this.dispatch(message);
    }
  }

  dispatch(message) {
    if (message.id !== undefined && message.method === undefined) {
      const entry = this.settle(message.id);
      if (entry === undefined) return;
      if (message.error) {
        entry.reject(new Error(`${message.error.message} (${message.error.code})`));
      } else {
        entry.resolve(message.result);
      }
      return;
    }
    if (message.id !== undefined) {
      const handler = this.requests.get(message.method);
      if (handler === undefined) {
        // Say so rather than answering `null`: a client that lies about a request it did not
        // understand is worse than one that admits it.
        this.resolveError(message.id, -32601, `${message.method} is not handled by this client`);
        return;
      }
      Promise.resolve()
        .then(() => handler(message.params))
        .then((result) => this.resolve(message.id, result))
        .catch((error) => {
          this.log(`${message.method} failed: ${error.message}`);
          this.resolveError(message.id, -32603, error.message);
        });
      return;
    }
    const handler = this.notifications.get(message.method);
    if (handler !== undefined) {
      Promise.resolve()
        .then(() => handler(message.params))
        .catch((error) => this.log(`${message.method} failed: ${error.message}`));
    }
  }
}

// ---------------------------------------------------------------------------
// the environment the server is started with
// ---------------------------------------------------------------------------

function expandHome(value) {
  if (value === '~') return os.homedir();
  if (value.startsWith('~/')) return path.join(os.homedir(), value.slice(2));
  return value;
}

/**
 * A key read from a file the user already has, rather than pasted into a setting.
 *
 * On macOS a GUI application does not inherit the login shell's environment, so a key
 * exported in `~/.bash_profile` or `~/.zshrc` is simply absent in Cursor and every decision
 * call fails with `model_error`. Pointing `jev.decide.apiKeyFile` at that profile puts it back
 * without the value ever entering a setting, a log line, or a commit.
 *
 * Two forms are accepted: the file is the key (its whole trimmed content, when it holds no
 * `=`), or it is a shell file and the key is the value of the last `NAME=value` —
 * `export NAME=value` and `NAME="value"` both read. The *name* is `jev.decide.apiKeyEnv`,
 * because that is the variable the server looks the value up in (`api_key_env`, PROTOCOL §10).
 */
function keyFromFile(file, variable) {
  const text = fs.readFileSync(expandHome(file), 'utf8').trim();
  if (!text.includes('=')) return text;
  const escaped = variable.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const assignment = new RegExp(`^\\s*(?:export\\s+)?${escaped}\\s*=\\s*(.*)$`);
  let value = '';
  for (const line of text.split('\n')) {
    const match = assignment.exec(line);
    if (match !== null) value = match[1].trim();
  }
  if (value.length > 1 && (value.startsWith('"') && value.endsWith('"'))) {
    return value.slice(1, -1);
  }
  if (value.length > 1 && (value.startsWith("'") && value.endsWith("'"))) {
    return value.slice(1, -1);
  }
  return value;
}

/**
 * The environment for the child process.
 *
 * The server reads its endpoints from its own environment (§10), and this is where the
 * extension's settings become that environment: a setting that is non-empty wins over the
 * ambient variable, and an empty one leaves the ambient variable alone (the server ignores
 * empty values itself, for the same reason). `JEV_REVIEW_MODEL` and the other variables §10
 * lists are not contributed here; they are honoured when they are already in the environment
 * the extension host inherited.
 */
function childEnvironment(settings, log) {
  const env = { ...process.env };
  const put = (name, value) => {
    if (typeof value === 'string' && value.trim() !== '') env[name] = value.trim();
  };
  put('JEV_BASE_URL', settings.chat.baseUrl);
  put('JEV_MODEL', settings.chat.model);
  put('JEV_API_KEY_ENV', settings.chat.apiKeyEnv);
  put('JEV_DECIDE_BASE_URL', settings.decide.baseUrl);
  put('JEV_DECIDE_MODEL', settings.decide.model);
  put('JEV_DECIDE_WIRE', settings.decide.wire);
  put('JEV_DECIDE_API_KEY_ENV', settings.decide.apiKeyEnv);
  if (Number.isInteger(settings.decide.timeoutMs) && settings.decide.timeoutMs > 0) {
    env.JEV_DECIDE_TIMEOUT_MS = String(settings.decide.timeoutMs);
  }
  fillKey(env, settings.decide.apiKeyEnv || DEFAULT_DECIDE_KEY_VARIABLE, settings.decide.apiKeyFile, log);
  fillKey(env, settings.chat.apiKeyEnv || 'OPENROUTER_API_KEY', settings.chat.apiKeyFile, log);
  return env;
}

function fillKey(env, variable, file, log) {
  if (typeof file !== 'string' || file.trim() === '') return;
  const name = variable.trim();
  try {
    const key = keyFromFile(file.trim(), name);
    if (key === '') {
      log(`${file} holds no assignment for ${name}, so ${name} stays unset`);
      return;
    }
    // The value is never logged, and never put anywhere but this process's environment.
    env[name] = key;
    log(`${name} read from ${file} (${key.length} bytes)`);
  } catch (error) {
    log(`cannot read the key file ${file}: ${error.message}`);
  }
}

// ---------------------------------------------------------------------------
// rendering what a command answered
// ---------------------------------------------------------------------------

/**
 * Commands whose answer is a value to glance at rather than text to read.
 *
 * These report to *Output → Jev* and never open a document. `jev.inspect` is the case that
 * forced the distinction: its whole answer is `{findings, considered, candidates, skipped}` —
 * counts and skip codes, 280 bytes on the fixture — so a document for it replaced what the user
 * was looking at with six lines they then had to close, which is precisely what a command you
 * run *while* looking at a file must not do. `jev.status` is a numbers snapshot, and
 * `jev.recompute` and `jev.revert` answer one value each. Everything else answers something
 * meant to be read, and `jev.artifacts.viewColumn` says where that document goes.
 */
const SUMMARY_COMMANDS = new Set(['jev.inspect', 'jev.status', 'jev.recompute', 'jev.revert']);

/** One line for a summary command, from the fields the server actually returns. */
function summaryLine(command, value) {
  const parts = [];
  if (typeof value.version === 'string') parts.push(`jev ${value.version}`);
  if (typeof value.enabled === 'boolean') parts.push(value.enabled ? 'enabled' : 'disabled');
  if (Array.isArray(value.findings)) {
    parts.push(value.findings.length === 0 ? 'no findings' : `${value.findings.length} finding(s)`);
  }
  if (typeof value.considered === 'number') parts.push(`${value.considered} rule(s) considered`);
  if (typeof value.candidates === 'number') parts.push(`${value.candidates} candidate(s)`);
  for (const skipped of value.skipped ?? []) parts.push(`skipped ${skipped.code}`);
  if (typeof value.documents === 'number') parts.push(`${value.documents} open document(s)`);
  if (typeof value.recomputed === 'boolean') parts.push(value.recomputed ? 'recomputed' : 'nothing to recompute');
  if (typeof value.reverted === 'string') parts.push(`reverted ${value.reverted.split('/').pop()}`);
  if (parts.length === 0) parts.push(command.replace(/^jev\./, ''));
  return parts.join(' · ');
}

/**
 * The body of an artifact document.
 *
 * An artifact carries `markdown` when the server has prose to give (`explain`, `ask`,
 * `followup`, `usage`); a plan carries `steps` and a review or an inspect carries `findings`,
 * and those two are formatted here rather than shown as raw JSON. Anything else —
 * `jev.status`, `jev.session` — is shown as it arrived, which is honest and needs no invented
 * renderer. (The Neovim plugin renders a plan as an interactive step buffer; this one does
 * not, and `docs/CURSOR.md` says so.)
 */
function artifactBody(value) {
  if (value === null || value === undefined) return '';
  if (typeof value === 'string') return value;
  if (typeof value.markdown === 'string' && value.markdown.length > 0) return value.markdown;
  if (Array.isArray(value.steps)) return planBody(value);
  if (Array.isArray(value.findings) || Array.isArray(value.skipped)) return findingsBody(value);
  return JSON.stringify(value, null, 2);
}

function planBody(plan) {
  const lines = ['# jev plan'];
  if (plan.goal) lines.push('', `Goal: ${plan.goal}`);
  if (plan.language) lines.push(`Language: ${plan.language}`);
  for (const step of plan.steps) {
    lines.push('', `## ${step.n}. ${step.title}  [${step.verb} · ${step.status}]`);
    if (step.rationale) lines.push('', step.rationale);
    for (const target of step.targets ?? []) {
      const line = target.range?.start?.line;
      const at = line === undefined ? '' : ` line ${line + 1}`;
      lines.push('', `- \`${target.uri}\`${at} (version ${target.version})`);
    }
  }
  return lines.join('\n');
}

function findingsBody(result) {
  const counts = [];
  if (typeof result.considered === 'number') counts.push(`considered ${result.considered}`);
  if (typeof result.candidates === 'number') counts.push(`candidates ${result.candidates}`);
  if (typeof result.from_cache === 'boolean') {
    counts.push(result.from_cache ? 'from cache' : 'fresh');
  }
  const findings = result.findings ?? [];
  const lines = ['# jev', '', counts.join(' · '), ''];
  lines.push(findings.length === 0 ? 'no findings' : `${findings.length} finding(s)`);
  for (const finding of findings) {
    lines.push('', `## line ${finding.line + 1} — ${finding.label}`, '', finding.detail ?? '');
  }
  for (const skipped of result.skipped ?? []) {
    lines.push('', `- skipped \`${skipped.code}\`: ${skipped.detail}`);
  }
  return lines.join('\n');
}

// ---------------------------------------------------------------------------
// the commands, and what each needs from the editor
// ---------------------------------------------------------------------------

/**
 * The commands that cannot be answered without a file — the whole refusal list, now that the
 * "any non-file active document" guard is gone.
 *
 * Each entry's `build` reads `document.uri` or the cursor, which is what makes a file a
 * requirement rather than a preference: `inspect` and `review` name a document; `explain`,
 * `followup` and `plan` name the scope at the cursor; `ask` names the file its question is
 * about. The rest — `status`, `recompute`, `session`, `usage`, `revert` — read no file, so an
 * open artifact or an empty window must not refuse them: `status` and `session` in particular
 * are how a user checks whether anything is working at all.
 */
const NEEDS_A_FILE = new Set([
  'jev.inspect',
  'jev.review',
  'jev.explain',
  'jev.ask',
  'jev.followup',
  'jev.plan',
]);

const COMMANDS = [
  { id: 'jev.status', title: 'Jev: status — queue, budgets, endpoints in force', build: () => ({}) },
  { id: 'jev.recompute', title: 'Jev: recompute every open file', build: () => ({}) },
  {
    id: 'jev.inspect',
    title: 'Jev: inspect this file — run the rules pass now',
    // `force` is the point: without it a document git does not report as changed is not
    // inspected at all (PROTOCOL §6), which looks exactly like a clean file.
    build: (document) => ({ path: document.uri.toString(), force: true }),
  },
  {
    id: 'jev.review',
    title: 'Jev: review this file — the chat tier, now',
    build: (document) => ({ uri: document.uri.toString() }),
  },
  {
    id: 'jev.explain',
    title: 'Jev: explain the scope at the cursor',
    build: (document, editor) => ({ uri: document.uri.toString(), line: editor.selection.active.line }),
  },
  {
    id: 'jev.ask',
    title: 'Jev: ask a question about this file',
    prompt: { message: 'What should jev answer?', placeholder: 'e.g. what does this module do?' },
    build: (document, editor, answer) => ({
      question: answer,
      uri: document.uri.toString(),
    }),
  },
  {
    id: 'jev.followup',
    title: 'Jev: ask a follow-up about the scope at the cursor',
    prompt: { message: 'Follow-up question', placeholder: 'e.g. is this reachable?' },
    build: (document, editor, answer) => {
      // §6: when the cursor is on a finding, that finding's id is looked up and put in the
      // prompt. The finding id is the diagnostic's `code` (PROTOCOL §9), so it is read back
      // from the diagnostic the editor is already showing.
      const on = vscode.languages
        .getDiagnostics(document.uri)
        .find((d) => d.source === DIAGNOSTIC_SOURCE && d.range.contains(editor.selection.active));
      const code = on?.code;
      const id = typeof code === 'object' && code !== null ? code.value : code;
      return {
        uri: document.uri.toString(),
        line: editor.selection.active.line,
        question: answer,
        finding_id: typeof id === 'string' ? id : undefined,
      };
    },
  },
  {
    id: 'jev.plan',
    title: 'Jev: plan a goal at the cursor',
    prompt: {
      message: 'Goal',
      placeholder: 'e.g. make this handler return errors instead of panicking',
    },
    build: (document, editor, answer) => ({
      goal: answer,
      scope: { uri: document.uri.toString(), line: editor.selection.active.line },
    }),
  },
  { id: 'jev.session', title: 'Jev: session log', build: () => ({ limit: 50 }) },
  {
    id: 'jev.usage',
    title: 'Jev: usage — what was offered and what was done with it',
    build: () => ({}),
  },
  {
    id: 'jev.revert',
    title: 'Jev: revert an applied edit by id',
    prompt: { message: 'Edit id to revert', placeholder: 'the `edit_id` a plan step carried' },
    build: (document, editor, answer) => ({ edit_id: answer }),
  },
];

// ---------------------------------------------------------------------------
// the client
// ---------------------------------------------------------------------------

class JevClient {
  constructor(extensionUri) {
    this.output = vscode.window.createOutputChannel('Jev');
    this.diagnostics = vscode.languages.createDiagnosticCollection('jev');
    this.status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 0);
    this.status.name = 'Jev';
    // VS Code has no diagnostic gutter of its own — a finding is a squiggle, a hover and a row
    // in Problems, and nothing in the margin. The Neovim plugin puts a sign there; this is that
    // sign, as a decoration. It renders the same `Diagnostic` the squiggle renders and asks
    // nothing new of the server (PROTOCOL.md §9), which is what makes it presentation rather
    // than a feature the protocol does not have.
    this.gutter = vscode.window.createTextEditorDecorationType({
      gutterIconPath: vscode.Uri.joinPath(extensionUri, 'media', 'jev.svg'),
      gutterIconSize: 'contain',
    });
    // Artifacts are served through a content provider rather than as untitled documents:
    // an untitled document made from `content` is *dirty*, so `⌘W` on an explanation raised a
    // save prompt and `⌘S` raised a Save As sheet — a stray modal where a scratch buffer should
    // be. A `jev-artifact:` document is read-only, has a name, and closes with one keystroke.
    this.artifacts = new Map();
    this.artifactCount = 0;
    this.models = new Map(); // uri -> the text the server holds
    this.resultIds = new Map(); // uri -> the last `resultId` a pull returned
    this.progress = 0;
    this.connection = undefined;
    this.ready = false;
    this.lensChanged = new vscode.EventEmitter();
    this.hintsChanged = new vscode.EventEmitter();
  }

  log(message) {
    this.output.appendLine(message);
  }

  // -- lifecycle ----------------------------------------------------------

  async start() {
    const settings = this.settings();
    const command = settings.server.path;
    const args = settings.server.args;
    const folder = vscode.workspace.workspaceFolders?.[0];
    this.log(`--- starting ${command} ${args.join(' ')}${folder ? ` (cwd ${folder.uri.fsPath})` : ''}`);

    const connection = new Connection({
      command,
      args,
      cwd: folder?.uri.fsPath,
      env: childEnvironment(settings, (line) => this.log(line)),
      log: (line) => this.log(line),
      failure: (message) =>
        vscode.window.showErrorMessage(
          `Jev: cannot start \`${command}\`: ${message}. Set \`jev.server.path\` to the server binary.`,
        ),
      onExit: (code, signal) => {
        this.ready = false;
        this.clearDiagnostics();
        this.log(`--- jev-lsp exited (${signal ?? code})`);
      },
    });
    this.connection = connection;

    // Handlers first: `initialized` makes the server pull `workspace/configuration`, and a
    // request that arrives before its handler is registered is answered `methodNotFound`.
    connection.onRequest('workspace/configuration', () => [this.settings().section]);
    connection.onRequest('window/workDoneProgress/create', () => null);
    connection.onRequest('workspace/applyEdit', (params) => this.applyEdit(params.edit));
    connection.onRequest('window/showDocument', (params) => this.showDocument(params));
    connection.onRequest('window/showMessageRequest', () => null);
    connection.onRequest('client/registerCapability', () => null);
    connection.onRequest('client/unregisterCapability', () => null);

    connection.onNotification('textDocument/publishDiagnostics', (params) => this.publish(params));
    connection.onNotification('$/progress', (params) => this.onProgress(params));
    // `workspace/*/refresh` are server->client *requests* in LSP 3.17, not notifications: each
    // is answered with `null` once the re-pull has happened. Registering them as notifications
    // means the server is told `methodNotFound` and the findings it just computed never reach
    // the editor, which is the exact failure PROTOCOL.md §2 describes.
    connection.onRequest('workspace/diagnostic/refresh', async () => {
      await this.pullOpen();
      return null;
    });
    connection.onRequest('workspace/codeLens/refresh', () => {
      this.lensChanged.fire();
      return null;
    });
    connection.onRequest('workspace/inlayHint/refresh', () => {
      this.hintsChanged.fire();
      return null;
    });
    connection.onNotification('window/logMessage', (params) =>
      this.log(`${logLevelName(params.type)}: ${params.message}`),
    );
    connection.onNotification('window/showMessage', (params) =>
      vscode.window.showInformationMessage(`Jev: ${params.message}`),
    );

    await connection.request('initialize', initializeParams(folder));
    this.ready = true;
    connection.notify('initialized', {});
    for (const document of vscode.workspace.textDocuments) this.didOpen(document);
  }

  async stop() {
    const connection = this.connection;
    if (connection === undefined) return;
    this.connection = undefined;
    this.ready = false;
    try {
      // No `params` at all. `shutdown` and `exit` take none, and sending `null` is rejected:
      // “Unexpected params: null (-32602)”, measured — which left every session ending in a
      // kill rather than a shutdown.
      await connection.request('shutdown');
    } catch (error) {
      this.log(`shutdown: ${error.message}`);
    }
    connection.notify('exit');
    await new Promise((resolve) => setTimeout(resolve, 250));
    if (!connection.closed) connection.child.kill();
    this.status.hide();
  }

  async restart() {
    this.clearDiagnostics();
    this.models.clear();
    this.resultIds.clear();
    await this.stop();
    await this.start();
  }

  // -- settings -----------------------------------------------------------

  settings() {
    const config = vscode.workspace.getConfiguration('jev');
    return {
      server: {
        path: config.get('server.path', 'jev-lsp'),
        args: config.get('server.args', ['--stdio']),
      },
      chat: {
        baseUrl: config.get('chat.baseUrl', ''),
        model: config.get('chat.model', ''),
        apiKeyEnv: config.get('chat.apiKeyEnv', ''),
        apiKeyFile: config.get('chat.apiKeyFile', ''),
      },
      decide: {
        baseUrl: config.get('decide.baseUrl', ''),
        model: config.get('decide.model', ''),
        wire: config.get('decide.wire', ''),
        apiKeyEnv: config.get('decide.apiKeyEnv', ''),
        apiKeyFile: config.get('decide.apiKeyFile', ''),
        timeoutMs: config.get('decide.timeoutMs', 0),
      },
      // The server's own `jev` configuration section (PROTOCOL §10), passed through verbatim:
      // budget, triggers, rules, noise and the model tiers live here, and this extension
      // invents no key of its own inside it.
      section: config.get('settings', {}),
      codeLens: config.get('codeLens.enabled', true),
      inlayHints: config.get('inlayHints.enabled', false),
      artifacts: { viewColumn: config.get('artifacts.viewColumn', 'active') },
    };
  }

  /**
   * A setting changed. The endpoints are read by the server from the environment it was
   * started with, so anything that maps to one needs a restart; `jev.settings` travels over
   * `workspace/configuration` and needs only the notification (§10 `didChangeConfiguration`).
   */
  async reconfigure(event) {
    if (!event.affectsConfiguration('jev')) return;
    const needsRestart = ['jev.server', 'jev.chat', 'jev.decide'].some((section) =>
      event.affectsConfiguration(section),
    );
    if (needsRestart) {
      await this.restart();
      return;
    }
    this.connection?.notify('workspace/didChangeConfiguration', {
      settings: this.settings().section,
    });
  }

  // -- document sync ------------------------------------------------------

  didOpen(document) {
    if (!isAnalysable(document)) return;
    this.models.set(document.uri.toString(), new TextModel(document.getText()));
    this.connection?.notify('textDocument/didOpen', {
      textDocument: {
        uri: document.uri.toString(),
        languageId: document.languageId,
        version: document.version,
        text: document.getText(),
      },
    });
    this.pull(document);
  }

  didChange(event) {
    if (!isAnalysable(event.document)) return;
    const changes = event.contentChanges;
    if (changes.length === 0) return;
    const uri = event.document.uri.toString();
    const model = this.models.get(uri);
    let wire = null;
    if (model !== undefined) {
      const converted = changes.map((change) => ({
        range: change.range === undefined ? undefined : toWireRange(event.document, change.range),
        rangeLength: change.rangeLength,
        text: change.text,
      }));
      for (const change of changes) model.apply(change);
      if (model.lines.length === event.document.lineCount) wire = converted;
    }
    if (wire === null) {
      // No model, or the model and the editor disagree about how many lines there are: resend
      // the whole text (a change with no `range` replaces the document) and start the model
      // again. An incremental change computed against a diverged copy would put every later
      // range in the wrong place, silently.
      this.log(`resynchronising ${uri} with the whole text`);
      this.models.set(uri, new TextModel(event.document.getText()));
      wire = [{ text: event.document.getText() }];
    }
    this.connection?.notify('textDocument/didChange', {
      textDocument: { uri, version: event.document.version },
      contentChanges: wire,
    });
  }

  didSave(document) {
    if (!isAnalysable(document)) return;
    // `textDocumentSync.save.includeText` is false, so no `text` travels with this. This is the
    // notification the rules pass runs on: a client that never sends it sees nothing at all,
    // which is indistinguishable from a clean file.
    this.connection?.notify('textDocument/didSave', {
      textDocument: { uri: document.uri.toString() },
    });
  }

  didClose(document) {
    if (!isAnalysable(document)) return;
    const uri = document.uri.toString();
    this.models.delete(uri);
    this.resultIds.delete(uri);
    this.setDiagnostics(document.uri, []);
    this.connection?.notify('textDocument/didClose', { textDocument: { uri } });
  }

  // -- diagnostics --------------------------------------------------------

  pullOpen() {
    return Promise.all(vscode.workspace.textDocuments.map((document) => this.pull(document)));
  }

  async pull(document) {
    if (!this.ready || !isAnalysable(document)) return;
    const uri = document.uri.toString();
    const params = { textDocument: { uri }, identifier: DIAGNOSTIC_SOURCE };
    const previous = this.resultIds.get(uri);
    if (previous !== undefined) params.previousResultId = previous;
    try {
      const report = await this.connection.request('textDocument/diagnostic', params);
      const current = vscode.workspace.textDocuments.find((d) => d.uri.toString() === uri);
      if (current === undefined) return;
      if (report.kind === 'unchanged') return;
      if (report.resultId === undefined || report.resultId === null) {
        this.resultIds.delete(uri);
      } else {
        this.resultIds.set(uri, report.resultId);
      }
      this.setDiagnostics(
        current.uri,
        (report.items ?? []).map((item) => toVscodeDiagnostic(current, item)),
      );
    } catch (error) {
      this.log(`diagnostics for ${uri}: ${error.message}`);
    }
  }

  /**
   * Every write to the collection repaints the gutter.
   *
   * There is no event to hook: Cursor's `DiagnosticCollection` has no `onDidChange` — its
   * interface in the app bundle's `vscode-dts/vscode.d.ts` stops at `get` — so
   * `client.diagnostics.onDidChange is not a function` is what an extension that assumes the
   * upstream API gets, as an *activation failure*: the whole extension, not just the gutter.
   * (Measured: `Activating extension makefunstuff.jev failed due to an error`.)
   */
  setDiagnostics(uri, diagnostics) {
    this.diagnostics.set(uri, diagnostics);
    this.paintGutter();
  }

  clearDiagnostics() {
    this.diagnostics.clear();
    this.paintGutter();
  }

  /**
   * Put the gutter mark on every visible editor's jev findings.
   *
   * Called from every write to the collection, and from `onDidChangeVisibleTextEditors` so a
   * split that appears later is painted too.
   */
  paintGutter() {
    for (const editor of vscode.window.visibleTextEditors) {
      const ranges = (this.diagnostics.get(editor.document.uri) ?? []).map((diagnostic) => {
        const line = diagnostic.range.start.line;
        return new vscode.Range(line, 0, line, 0);
      });
      editor.setDecorations(this.gutter, ranges);
    }
  }

  /** The push half: the server publishes an empty list on `didClose`, and whatever else it likes. */
  publish(params) {
    const document = vscode.workspace.textDocuments.find((d) => d.uri.toString() === params.uri);
    if (document === undefined) return;
    this.resultIds.delete(params.uri);
    this.setDiagnostics(
      document.uri,
      (params.diagnostics ?? []).map((item) => toVscodeDiagnostic(document, item)),
    );
  }

  // -- progress -----------------------------------------------------------

  onProgress(params) {
    const value = params.value ?? {};
    if (value.kind === 'begin') {
      this.progress += 1;
      this.status.text = `$(sync~spin) ${value.title ?? 'jev'}`;
      this.status.tooltip = 'jev';
      this.status.show();
      return;
    }
    if (value.kind === 'report') {
      // PROTOCOL §3.5: `message` is the human line and the partial artifact travels in `data`.
      // This client shows the message; it does not stream the partial text into a buffer, and
      // `docs/CURSOR.md` says so.
      this.status.text = `$(sync~spin) ${value.message ?? value.title ?? 'jev'}`;
      return;
    }
    if (value.kind === 'end') {
      this.progress = Math.max(0, this.progress - 1);
      if (this.progress === 0) this.status.hide();
    }
  }

  // -- the server's own requests ------------------------------------------

  async applyEdit(edit) {
    let workspaceEdit;
    try {
      workspaceEdit = toWorkspaceEdit(edit);
    } catch (error) {
      this.log(`refusing an edit: ${error.message}`);
      return { applied: false, failureReason: error.message };
    }
    const applied = await vscode.workspace.applyEdit(workspaceEdit);
    if (!applied) this.log('the editor refused an edit the server applied');
    return applied
      ? { applied: true }
      : { applied: false, failureReason: 'the editor refused the edit' };
  }

  /**
   * `window/showDocument`.
   *
   * The server asks for `jev://artifact/<id>` and does not send the markdown with the request —
   * the artifact stays in its own store — so a client with no `jev://` resolver cannot render
   * it. The answer is `{success: false}` plus a line in the output channel; the same artifact is
   * reachable whole through the commands that return it (`jev.explain`, `jev.ask`, `jev.plan`).
   */
  showDocument(params) {
    this.log(`the server asked to open ${params.uri}, which this client cannot resolve`);
    vscode.window.setStatusBarMessage(
      'Jev: the server produced an artifact — open it with `Jev: explain…` or `Jev: ask…`',
      8000,
    );
    return { success: false };
  }

  // -- commands -----------------------------------------------------------

  /**
   * A wire action as a `vscode.CodeAction`.
   *
   * `previous` is the action this one resolved from, so its presence means this is the
   * `codeAction/resolve` answer rather than the first `textDocument/codeAction` list. VS Code
   * calls `resolveCodeAction` for every action that has no `edit` ("a code action that has an
   * edit will not be resolved", `CodeActionProvider.resolveCodeAction`), so attaching the
   * fallback command here — and not before — means it is added exactly when the resolve came
   * back with nothing to apply.
   */
  toVscodeCodeAction(action, previous) {
    const out = previous ?? new vscode.CodeAction(action.title, kindOf(action.kind));
    out.title = action.title;
    if (action.kind !== undefined) out.kind = kindOf(action.kind);
    if (action.isPreferred !== undefined) out.isPreferred = action.isPreferred;
    out.disabled = action.disabled === undefined ? undefined : { reason: action.disabled.reason };
    if (action.data !== undefined) out.data = action.data;
    out.edit = undefined;
    out.command = action.command === undefined ? undefined : toVscodeCommand(action.command);
    if (action.edit !== undefined) {
      try {
        out.edit = toWorkspaceEdit(action.edit);
      } catch (error) {
        // The edit is dropped rather than the action: the title still names the intent, and the
        // reason lands in the output channel instead of a modal nobody can act on.
        this.log(`dropping the edit on "${action.title}": ${error.message}`);
      }
    }
    if (previous !== undefined && out.edit === undefined && out.command === undefined) {
      out.command = {
        command: 'jev.plugin.resolve',
        title: action.title,
        arguments: [action],
      };
    }
    return out;
  }

  /**
   * `workspace/executeCommand` for one of the server's commands, then show what came back.
   *
   * `PROTOCOL.md` §7: a command answers either a `Result` envelope or an artifact. An artifact
   * carries `markdown`; a plan carries `steps`; a review or inspect carries `findings`;
   * everything else is shown as it arrived.
   */
  async runCommand(command, argument) {
    const token = `jev-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
    let value;
    try {
      value = await this.connection.request('workspace/executeCommand', {
        command,
        arguments: [argument],
        workDoneToken: token,
      });
    } catch (error) {
      vscode.window.showErrorMessage(`Jev: ${command} failed — ${error.message}`);
      return;
    }
    if (value !== null && typeof value === 'object' && value.ok === false) {
      const failure = value.error ?? {};
      vscode.window.showErrorMessage(
        `Jev: ${command} — ${failure.code ?? 'error'}: ${failure.message ?? 'no message'}`,
      );
      return;
    }
    const body = artifactBody(value);
    this.log(`${command} → ${body.length} bytes`);

    if (SUMMARY_COMMANDS.has(command)) {
      // A summary goes to the channel and says so in one line with a way in. It never moves the
      // editor's layout: the answer is smaller than the interruption would be.
      this.log(body);
      const answer = await vscode.window.showInformationMessage(
        `Jev: ${summaryLine(command, value)}`,
        'Details',
      );
      if (answer === 'Details') this.output.show();
      return body;
    }

    const preference = this.settings().artifacts.viewColumn;
    if (preference === 'output') {
      this.log(body);
      this.output.show(true);
      return body;
    }

    const uri = vscode.Uri.parse(
      `jev-artifact:/${command.replace(/^jev\./, '')}-${(this.artifactCount += 1)}.md`,
    );
    this.artifacts.set(uri.toString(), `${body}\n`);
    const document = await vscode.workspace.openTextDocument(uri);
    await vscode.window.showTextDocument(document, {
      preview: false,
      // `ViewColumn.Active` and not `ViewColumn.Beside`. With a single group open, `Beside` is
      // a *split*: the editor rearranges itself around an answer to a question the user asked,
      // which is the complaint this default exists to fix. `beside` is still available.
      viewColumn:
        preference === 'beside' ? vscode.ViewColumn.Beside : vscode.ViewColumn.Active,
    });
    return body;
  }

  // -- the actions behind a lens ------------------------------------------

  documentByUri(uri) {
    return vscode.workspace.textDocuments.find((d) => d.uri.toString() === uri);
  }

  /** The server's actions at one position, exactly as the lightbulb asks for them. */
  async codeActionsAt(document, line) {
    const position = new vscode.Position(line, 0);
    const params = {
      textDocument: { uri: document.uri.toString() },
      range: toWireRange(document, new vscode.Range(position, position)),
      context: {
        diagnostics: vscode.languages
          .getDiagnostics(document.uri)
          .filter((d) => d.range.contains(position))
          .map((d) => toWireDiagnostic(document, d)),
        triggerKind: 1,
      },
    };
    return (await this.connection.request('textDocument/codeAction', params)) ?? [];
  }

  /**
   * The `jev.plugin.pick` lens: `jev: N finding(s) · fix` on a declaration that has findings.
   *
   * The Neovim plugin opens its own picker here (`picker.action()`). This is that picker
   * expressed with the surface VS Code already has: the server's `textDocument/codeAction`
   * list at the lens's line, in a quick pick, and the chosen action resolved and applied. There
   * is no `jev.actions` or `jev.action` command to call — `COMMANDS` in the server and
   * PROTOCOL.md §6 both stop at the fifteen that are listed there — so the picker is built from
   * the client-to-server code action pair, which is what those methods are for.
   */
  async pickAction(target) {
    const document = this.documentByUri(target.uri);
    if (document === undefined) {
      this.log(`pick: no open editor for ${target.uri}; nothing to offer`);
      return;
    }
    const editor = await vscode.window.showTextDocument(document, { preview: false });
    editor.selection = new vscode.Selection(target.line, 0, target.line, 0);

    const actions = await this.codeActionsAt(document, target.line);
    this.log(
      `pick at ${document.uri.path}:${target.line + 1} — the server offered ` +
        `${actions.length} action(s): ${actions.map((a) => a.title).join(' | ') || 'none'}`,
    );
    if (actions.length === 0) {
      vscode.window.showInformationMessage(
        `Jev: no action at line ${target.line + 1}. Save the file first — the ambient pass runs on save.`,
      );
      return;
    }
    const choice = await vscode.window.showQuickPick(
      actions.map((action) => ({
        label: action.title,
        description: action.kind,
        detail: action.disabled?.reason,
        action,
      })),
      {
        placeHolder: `jev at line ${target.line + 1}`,
        matchOnDescription: true,
        matchOnDetail: true,
      },
    );
    if (choice === undefined) {
      this.log('pick cancelled');
      return;
    }
    await this.applyServerAction(choice.action);
  }

  /**
   * Resolve an action and make something happen.
   *
   * Three outcomes and no fourth: an `edit` goes to `workspace/applyEdit`'s sibling,
   * `vscode.workspace.applyEdit`; a `command` is run through the editor; and a verb that
   * produces an artifact is fetched with the command that returns it whole, because the server
   * signals that case with `window/showDocument jev://artifact/<id>` and does not send the
   * markdown (PROTOCOL §3.4.1, §7). Anything left over is said out loud rather than dropped.
   */
  async applyServerAction(action) {
    let resolved = action;
    if (action.data !== undefined && action.edit === undefined && action.command === undefined) {
      resolved = await this.connection.request('codeAction/resolve', action);
    }
    if (resolved.edit !== undefined) {
      try {
        const applied = await vscode.workspace.applyEdit(toWorkspaceEdit(resolved.edit));
        this.log(`applied "${resolved.title}" — the editor reported ${applied}`);
        if (!applied) vscode.window.showWarningMessage(`Jev: the editor refused the edit for ${resolved.title}.`);
        return;
      } catch (error) {
        this.log(`could not apply "${resolved.title}": ${error.message}`);
        vscode.window.showWarningMessage(`Jev: ${resolved.title} — ${error.message}`);
        return;
      }
    }
    if (resolved.command !== undefined) {
      this.log(`running ${resolved.command.command} for "${resolved.title}"`);
      await vscode.commands.executeCommand(
        resolved.command.command,
        ...(resolved.command.arguments ?? []),
      );
      return;
    }
    const verb = resolved.data?.verb ?? action.data?.verb;
    const uri = resolved.data?.doc?.uri ?? action.data?.doc?.uri;
    const line = resolved.data?.scope?.start_line ?? action.data?.scope?.start_line;
    if (verb === 'explain' && typeof uri === 'string' && typeof line === 'number') {
      await this.runCommand('jev.explain', { uri, line });
      return;
    }
    if (verb === 'review' && typeof uri === 'string') {
      await this.runCommand('jev.review', { uri });
      return;
    }
    const why = resolved.disabled?.reason;
    this.log(`"${resolved.title}" resolved to ${verb ?? 'nothing'} with no edit and no command`);
    vscode.window.showWarningMessage(
      `Jev: ${resolved.title} — ${why ?? 'the server returned neither an edit nor a command'}`,
    );
  }
}

function logLevelName(type) {
  return { 1: 'error', 2: 'warning', 3: 'info', 4: 'log' }[type] ?? 'log';
}

/**
 * What this client advertises.
 *
 * `positionEncodings` says `utf-8` because that is what this client converts to and from; the
 * server pins the same value and does not read the list (PROTOCOL §2). `workspaceEdit` names
 * all three resource operations because §8 rule 4 sends edits that use them.
 * `showDocument.support` is `false` because this client cannot resolve `jev://`.
 */
function initializeParams(folder) {
  return {
    processId: process.pid,
    clientInfo: { name: CLIENT_NAME, version: CLIENT_VERSION },
    rootUri: folder?.uri.toString() ?? null,
    workspaceFolders:
      vscode.workspace.workspaceFolders?.map((f) => ({ uri: f.uri.toString(), name: f.name })) ??
      null,
    capabilities: {
      general: { positionEncodings: ['utf-8'] },
      window: { workDoneProgress: true, showMessage: {}, showDocument: { support: false } },
      workspace: {
        applyEdit: true,
        configuration: true,
        workspaceFolders: true,
        workspaceEdit: {
          documentChanges: true,
          resourceOperations: ['create', 'rename', 'delete'],
          failureHandling: 'abort',
        },
        diagnostics: { refreshSupport: true },
        codeLens: { refreshSupport: true },
        inlayHint: { refreshSupport: true },
      },
      textDocument: {
        synchronization: { dynamicRegistration: false, didSave: true },
        publishDiagnostics: { versionSupport: true },
        codeAction: {
          dynamicRegistration: false,
          isPreferredSupport: true,
          disabledSupport: true,
          dataSupport: true,
          resolveSupport: { properties: ['edit', 'command'] },
          codeActionLiteralSupport: {
            codeActionKind: { valueSet: CODE_ACTION_KINDS.map((kind) => kind.value) },
          },
        },
        diagnostic: { dynamicRegistration: false, relatedDocumentSupport: false },
        hover: { contentFormat: ['markdown', 'plaintext'] },
        codeLens: { dynamicRegistration: false },
        inlayHint: {
          dynamicRegistration: false,
          resolveSupport: { properties: ['text', 'tooltip', 'location'] },
        },
      },
    },
  };
}

// ---------------------------------------------------------------------------
// activation
// ---------------------------------------------------------------------------

function registerProviders(context, client) {
  // A file document, whatever its language: support is unconditional and language is metadata
  // (`docs/LANGUAGE.md` §1), so the selector is the scheme and not a list of languages.
  const selector = [{ scheme: 'file' }];

  context.subscriptions.push(
    vscode.languages.registerCodeActionsProvider(
      selector,
      {
        provideCodeActions: async (document, range, actionContext, token) => {
          if (!client.ready || !isAnalysable(document)) return [];
          const params = {
            textDocument: { uri: document.uri.toString() },
            range: toWireRange(document, range),
            context: {
              diagnostics: actionContext.diagnostics.map((d) => toWireDiagnostic(document, d)),
              // A **sequence**, not the single kind LSP 3.17 describes. tower-lsp 0.20 pins
              // `lsp-types 0.94.1`, whose `CodeActionContext.only` is
              // `Option<Vec<CodeActionKind>>` (`code_action.rs:338`), so a bare string here
              // fails deserialisation of the whole request: the server answers
              // `-32602 invalid type: string "quickfix", expected a sequence`, the provider
              // throws, and the editor reports "No quick fixes available" for a diagnostic that
              // has a fix. Measured both ways against the built binary.
              only: actionContext.only === undefined ? undefined : [actionContext.only.value],
              triggerKind:
                actionContext.triggerKind === vscode.CodeActionTriggerKind.Automatic ? 2 : 1,
            },
          };
          const actions = await client.connection.request('textDocument/codeAction', params, token);
          return (actions ?? []).map((action) => client.toVscodeCodeAction(action));
        },

        resolveCodeAction: async (action, token) => {
          if (!client.ready || action.data === undefined) return action;
          // The server reads `data` and nothing else from a resolve (`code_action_resolve`), so
          // round-tripping it is what makes the resolve stateless.
          const resolved = await client.connection.request(
            'codeAction/resolve',
            {
              title: action.title,
              kind: action.kind === undefined ? undefined : action.kind.value,
              data: action.data,
              isPreferred: action.isPreferred,
            },
            token,
          );
          return client.toVscodeCodeAction(resolved, action);
        },
      },
      CODE_ACTION_KINDS,
    ),

    vscode.languages.registerCodeLensProvider(selector, {
      onDidChangeCodeLenses: client.lensChanged.event,
      provideCodeLenses: async (document, token) => {
        if (!client.ready || !isAnalysable(document) || !client.settings().codeLens) return [];
        const lenses = await client.connection.request(
          'textDocument/codeLens',
          { textDocument: { uri: document.uri.toString() } },
          token,
        );
        return (lenses ?? []).map(
          (lens) =>
            new vscode.CodeLens(
              toVscodeRange(document, lens.range),
              lens.command === undefined ? undefined : toVscodeCommand(lens.command),
            ),
        );
      },
    }),

    vscode.languages.registerHoverProvider(selector, {
      provideHover: async (document, position, token) => {
        if (!client.ready || !isAnalysable(document)) return undefined;
        const hover = await client.connection.request(
          'textDocument/hover',
          {
            textDocument: { uri: document.uri.toString() },
            position: toWirePosition(document, position),
          },
          token,
        );
        if (hover === null || hover === undefined) return undefined;
        const contents = markdownOf(hover.contents);
        if (contents.length === 0) return undefined;
        return new vscode.Hover(
          contents,
          hover.range === undefined ? undefined : toVscodeRange(document, hover.range),
        );
      },
    }),

    vscode.languages.registerInlayHintsProvider(selector, {
      onDidChangeInlayHints: client.hintsChanged.event,
      provideInlayHints: async (document, range, token) => {
        if (!client.ready || !isAnalysable(document) || !client.settings().inlayHints) return [];
        const hints = await client.connection.request(
          'textDocument/inlayHint',
          { textDocument: { uri: document.uri.toString() }, range: toWireRange(document, range) },
          token,
        );
        return (hints ?? []).map((hint) => {
          const label =
            typeof hint.label === 'string'
              ? hint.label
              : hint.label.map((part) => part.value).join('');
          const out = new vscode.InlayHint(
            toVscodePosition(document, hint.position),
            label,
            hint.kind === 1 ? vscode.InlayHintKind.Type : vscode.InlayHintKind.Parameter,
          );
          if (hint.tooltip !== undefined) {
            out.tooltip = typeof hint.tooltip === 'string' ? hint.tooltip : hint.tooltip.value;
          }
          return out;
        });
      },
    }),

    // The lens commands live in the plugin's own `jev.plugin.` namespace, and the server
    // hardcodes the prefix: `code_lens` (`crates/jev-lsp/src/server.rs`) emits exactly
    // `jev.plugin.pick` (title `jev: N finding(s) · fix`) and `jev.plugin.explain` (title
    // `jev: explain`), with `arguments: [{uri, line}]`. Nothing in `initialize` or in the
    // settings schema renames them, and there is no `jev.actions`/`jev.action` command to fall
    // back on, so a client that does not register these two ids hands them to its own command
    // registry and the click does nothing at all.
    //
    // Neovim gets away without registering them because `nvim/lua/jev/init.lua` wraps
    // `vim.lsp.codelens.run` and dispatches the prefix itself. VS Code has no such hook, so the
    // names that reach it have to exist here.
    vscode.commands.registerCommand('jev.plugin.explain', async (...args) => {
      const target = lensTarget(args);
      if (target === undefined) {
        client.log(`jev.plugin.explain: unrecognised arguments ${JSON.stringify(args)}`);
        return;
      }
      client.log(`jev.plugin.explain → jev.explain at ${target.uri}:${target.line + 1}`);
      await client.runCommand('jev.explain', { uri: target.uri, line: target.line });
    }),

    vscode.commands.registerCommand('jev.plugin.pick', async (...args) => {
      const target = lensTarget(args);
      if (target === undefined) {
        client.log(`jev.plugin.pick: unrecognised arguments ${JSON.stringify(args)}`);
        return;
      }
      await client.pickAction(target);
    }),

    // This one is the client's own, not the server's: a resolved action that came back with
    // neither an `edit` nor a `command` is an artifact-producing verb, and VS Code would
    // otherwise render the item and do nothing when it is picked. Routing it here is what
    // `applyServerAction` is for.
    vscode.commands.registerCommand('jev.plugin.resolve', async (action) => {
      if (action === undefined || action.data === undefined) {
        client.log('jev.plugin.resolve: called without an action');
        return;
      }
      await client.applyServerAction(action);
    }),

    client.lensChanged,
    client.hintsChanged,
  );
}

let client;

async function activate(context) {
  client = new JevClient(context.extensionUri);
  context.subscriptions.push(client.output, client.diagnostics, client.status, client.gutter);
  context.subscriptions.push(
    vscode.window.onDidChangeVisibleTextEditors(() => client.paintGutter()),
    vscode.workspace.registerTextDocumentContentProvider('jev-artifact', {
      provideTextDocumentContent: (uri) => client.artifacts.get(uri.toString()) ?? '',
    }),
  );

  registerProviders(context, client);

  for (const definition of COMMANDS) {
    context.subscriptions.push(
      vscode.commands.registerCommand(definition.id, async () => {
        if (!client.ready) {
          vscode.window.showWarningMessage(`Jev: ${definition.id} — the server is not running.`);
          return;
        }
        const editor = vscode.window.activeTextEditor;
        const document = editor?.document;
        // One refusal, and it is the exclusion list that decides. The earlier version had a
        // second guard — "any active document that is not a file → refuse" — which fired for the
        // `jev-artifact:` document this extension opens itself, so after one answer *every*
        // command answered "open a file first", including `jev.status` and `jev.session`, which
        // `NEEDS_A_FILE` deliberately excludes because they read no file at all. The list
        // documented the intent and the guard defeated it.
        if (NEEDS_A_FILE.has(definition.id) && !isAnalysable(document)) {
          vscode.window.showWarningMessage('Jev: open a file first.');
          return;
        }
        let answer;
        if (definition.prompt !== undefined) {
          answer = await vscode.window.showInputBox({
            prompt: definition.prompt.message,
            placeHolder: definition.prompt.placeholder,
          });
          if (answer === undefined || answer.trim() === '') return;
        }
        await client.runCommand(definition.id, definition.build(document, editor, answer));
      }),
    );
  }

  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((document) => client.didOpen(document)),
    vscode.workspace.onDidChangeTextDocument((event) => client.didChange(event)),
    vscode.workspace.onDidSaveTextDocument((document) => client.didSave(document)),
    vscode.workspace.onDidCloseTextDocument((document) => client.didClose(document)),
    vscode.workspace.onDidChangeConfiguration((event) => client.reconfigure(event)),
    // The server's root is the first workspace folder (§ initialize), so a folder set that
    // changes its first entry is a different repository and needs a different process.
    vscode.workspace.onDidChangeWorkspaceFolders(() => client.restart()),
  );

  await client.start();
  client.pullOpen();
}

async function deactivate() {
  await client?.stop();
}

module.exports = { activate, deactivate };
