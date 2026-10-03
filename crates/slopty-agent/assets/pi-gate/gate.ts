// Slopty's permission gate for pi, driven over RPC (crates/slopty-agent/src/pi.rs).
//
// pi has no permission system, so every tool call waits here for the person's answer, asked
// through the RPC extension UI. The worker decides what to ask and what to let through; this
// only asks. It fails closed: anything but an allow blocks the call, and pi blocks a call whose
// handler throws.
//
// For a server task's thread the worker also hands it Slopty's tools (SLOPTY_PI_TOOLS, an mcp.json
// server entry), which it registers as one of pi's MCP servers. Their calls come through the gate
// like any other.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

// What the worker reads the dialog's title as: the gate's protocol and the call it is about.
const GATE = "slopty-gate/1";

// The name Slopty's tools go by: their tools are `mcp__slopty__…`.
const MCP_SERVER = "slopty";

export default function (pi: ExtensionAPI) {
	const tools = process.env.SLOPTY_PI_TOOLS;
	if (tools) {
		pi.registerMcpServer(MCP_SERVER, JSON.parse(tools));
	}
	pi.on("tool_call", async (event, ctx) => {
		if (ctx.mode !== "rpc") {
			return { block: true, reason: "Slopty's gate answers only over RPC" };
		}
		const tool = pi.getAllTools().find((t) => t.name === event.toolName);
		const title = JSON.stringify({
			gate: GATE,
			call: event.toolCallId,
			parent: event.parentToolCallId ?? null,
			tool: event.toolName,
			input: event.input,
			hints: tool?.annotations ?? {},
		});
		const answer = await ctx.ui.select(title, ["allow", "deny"], { signal: ctx.signal });
		if (answer === "allow") {
			return undefined;
		}
		// A deny may carry the person's reason on the lines after it.
		const reason = answer?.split("\n").slice(1).join("\n").trim();
		return { block: true, reason: reason || "The person did not allow it" };
	});
}
