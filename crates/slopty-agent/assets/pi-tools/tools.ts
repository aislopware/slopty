// Slopty's tools for a pi Slopty starts in a terminal (crates/slopty-agent/src/pi.rs).
//
// The worker hands them as an mcp.json server entry (SLOPTY_PI_TOOLS), registered as one of pi's
// own MCP servers. Nothing else: pi's TUI asks and answers everything else itself.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

// The name Slopty's tools go by: their tools are `mcp__slopty__…`.
const MCP_SERVER = "slopty";

export default function (pi: ExtensionAPI) {
	const tools = process.env.SLOPTY_PI_TOOLS;
	if (tools) {
		pi.registerMcpServer(MCP_SERVER, JSON.parse(tools));
	}
}
