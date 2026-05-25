# Claude Code

Claude Code is Cairn's v0.1 reference consumer. Register Cairn as a stdio MCP
server with one command:

```bash
cairn setup claude-code --vault work
```

By default this writes a local-scope entry in `~/.claude.json` for the current
project. Local scope is private to your user account and does not create a
shareable `.mcp.json`.

To write project scope explicitly:

```bash
cairn setup claude-code --scope project --vault work
```

Project scope writes `.mcp.json` in the project directory. Commit that file
only when the absolute binary and vault paths are intentional for the team, or
edit them to a team-supported path first.

Verify the registration:

```bash
cairn doctor claude-code
```

Remove only the Cairn server entry:

```bash
cairn setup claude-code --vault work remove
```

Cairn writes no API keys or provider credentials into Claude Code config. The
generated MCP entry uses an empty `env` object and launches:

```bash
cairn --vault <vault> mcp
```

See the [capability matrix](../reference/capability-matrix.md) for what ships in each release.
