// Slopty's live channel inside Claude Code: a pipe with no logic of its own.
//
// Claude Code loads this module for the sessions a Slopty worker starts (and for a `claude`
// typed into a Slopty terminal, through the shell integration). It passes a whitelist of the
// engine's events to the worker's mod socket, `SLOPTY_MOD_SOCKET`, as JSON over HTTP on that
// Unix socket, and changes nothing: every chunk and result goes on exactly as it came. The
// worker trusts nothing it says until the `hello` sent at `session.start` names a mod and a
// Claude Code version it knows; otherwise the hooks, the transcript and the status line stay
// the only sources, as they are wherever this module is not loaded.
//
// Events leave in order, in batches: one request in flight at a time, and whatever arrived
// meanwhile goes in the next. A worker that is gone costs one failed request per batch.
//
// Pinned to the Claude Code version the worker names in `MOD_CLAUDE_VERSIONS`; the plugin API
// is early access and changes between releases (`claude plugin validate` reads this file).

import type { EngineInterface, Register } from "claude-code";

/** What the worker's version gate checks beside Claude Code's own version. */
const MOD_PROTOCOL = 1;

/** The catalog last sent, as sent, so an unchanged one is not sent again. */
let catalogSent: string | undefined;

/**
 * Claude Code's own lists, for the worker's composer and model menu: every slash command the
 * person can run now (`$.command.list()`, built-in, plugin, user and MCP alike, in the
 * typeahead's order) and the aliases `/model` takes (the `/config` menu's `model` row, with the
 * one in use). Sent at the start and after each turn, when it changed.
 */
async function sendCatalog($: EngineInterface): Promise<void> {
  if (socket === undefined) return;
  const commands = await $.command.list();
  const row = (await $.config.list()).find((r) => r.key === "model");
  const catalog = {
    kind: "catalog",
    commands,
    models: row?.options ?? [],
    model: typeof row?.value === "string" ? row.value : undefined,
  };
  const text = JSON.stringify(catalog);
  if (text === catalogSent) return;
  catalogSent = text;
  send($, catalog);
}

/** Events waiting for the next request. */
let queue: unknown[] = [];
/** A request is on its way. */
let sending = false;
/** The worker's socket and the terminal session, once `session.start` read them. */
let socket: string | undefined;
let session: string | undefined;

async function flush($: EngineInterface): Promise<void> {
  if (sending || socket === undefined || queue.length === 0) return;
  sending = true;
  while (queue.length > 0) {
    const events = queue;
    queue = [];
    try {
      await $.http.fetch("http://slopty/v1/events", {
        method: "POST",
        socketPath: socket,
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ session, events }),
      });
    } catch {
      // The worker is away: these events are lost, the transcript still has them.
    }
  }
  sending = false;
}

function send($: EngineInterface, event: unknown): void {
  if (socket === undefined) return;
  queue.push(event);
  void flush($);
}

export const register: Register = (on) => {
  on("session.start", async ($, e, next) => {
    const result = await next(e);
    socket = await $.env.get("SLOPTY_MOD_SOCKET");
    session = await $.env.get("SLOPTY_SESSION");
    if (socket !== undefined) {
      const version = await $.session.version();
      send($, {
        kind: "hello",
        protocol: MOD_PROTOCOL,
        claude: version.version,
        sessionId: await $.session.id(),
        cwd: e.cwd,
        interactive: e.isInteractive,
      });
      await sendCatalog($);
    }
    return result;
  });

  on("turn.start", async ($, e, next) => {
    send($, { kind: "turn.start", turnId: e.turnId, text: e.text });
    return next(e);
  });

  on("turn.step", async function* ($, e, next) {
    const step = { turnId: e.turnId, step: e.index, agentId: e.agentId, model: e.model };
    send($, { kind: "step.start", ...step });
    const stream = next(e);
    for await (const chunk of stream) {
      switch (chunk.kind) {
        case "text":
        case "thinking":
          send($, { kind: chunk.kind, ...step, block: chunk.index, text: chunk.text });
          break;
        case "tool":
          send($, { kind: "tool", ...step, block: chunk.index, id: chunk.id, name: chunk.name });
          break;
        case "input":
          send($, { kind: "input", ...step, block: chunk.index, json: chunk.json });
          break;
        case "stop":
          send($, { kind: "stop", ...step, stopReason: chunk.stopReason, usage: chunk.usage });
          break;
      }
      yield chunk;
    }
    return await stream.result;
  });

  on("tool.call", async ($, e, next) => {
    send($, { kind: "tool.start", id: e.tool_use_id, tool: e.tool, agentId: e.agentId });
    const result = await next(e);
    send($, {
      kind: "tool.end",
      id: e.tool_use_id,
      agentId: e.agentId,
      denied: "deny" in result && result.deny !== undefined,
      isError: "isError" in result && result.isError === true,
    });
    return result;
  });

  on("turn.complete", async ($, e, next) => {
    send($, {
      kind: "turn.complete",
      turnId: e.turnId,
      agentId: e.agentId,
      reason: e.reason,
      durationMs: e.durationMs,
      usage: e.usage,
    });
    const result = await next(e);
    if (e.agentId === undefined) await sendCatalog($);
    return result;
  });

  on("session.measure", async ($, e, next) => {
    send($, { kind: "measure", context: e.context, rateLimits: e.rateLimits, cost: e.cost });
    return next(e);
  });

  on("session.end", async ($, e, next) => {
    send($, { kind: "bye" });
    return next(e);
  });
};
