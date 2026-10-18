# Tools via MCP

As a user, I want window and system functions exposed to the voice session through an integrated MCP server, so that the AI can do what the shell can do without a heavy extra process.

## Acceptance

- The voice session uses the OpenAI Realtime API.
- The tools are provided by an MCP server that belongs to the shell.
- The server runs integrated, not as its own process on a Node runtime.
- It can at least: run a shell command, switch between entries, turn Wi-Fi on and off, set up a Wi-Fi connection, set up a Bluetooth connection.
- The voice session can call these tools.
