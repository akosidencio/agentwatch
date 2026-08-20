# AgentWatch

See what your AI agents are doing on your machine.

Local-first activity monitoring for AI coding agents. Everything stays on your
machine: no network calls, no accounts, no telemetry leaving the box.

**Status:** phase 2 of 4. Claude Code on macOS only. See [PLAN.md](PLAN.md).

## What it does today

Records what Claude Code does — sessions, files read and written, shell
commands, MCP calls, and token usage — into a local SQLite database, and lets
you read it back.

Cost estimation and the menu bar are not here yet; they arrive in phase 4 and
later.

### Import your history first

```sh
agentwatch import
```

Reads every transcript Claude Code has already written, so the tool is useful
on the day you install it rather than after a week of collecting. Safe to run
repeatedly — nothing is double counted.

```sh
agentwatch tokens --today
agentwatch tokens --days 7 --by day
agentwatch tokens --all --by project --limit 10
agentwatch tokens --from 2026-08-01 --to 2026-08-21 --by model
agentwatch verify          # re-derive totals from source and report drift
```

## What it never records

- **Prompt text.** Only a character count and a SHA-256 digest, so repeated
  prompts can be recognized without anyone being able to read them.
- **Tool output.** `tool_response` is not deserialized at all, so file contents
  and command output never reach a Rust value, let alone the database.
- **File contents** from `Write` and `Edit` payloads.

Shell command lines *are* recorded, because a command monitor that hides
commands is pointless. That means a command containing a secret gets stored.
Scrubbing for the obvious shapes lands in phase 4.

## Build

```sh
cargo build --release
cargo test
```

## Run

```sh
# start the daemon in one terminal
./target/release/agentwatch-daemon

# in another, print the settings you need
./target/release/agentwatch hook-config --binary "$PWD/target/release/agentwatch-hook"
```

`hook-config` prints and nothing else. Copy the block into
`~/.claude/settings.json` yourself, merging with any hooks you already have.
Automatic installation arrives in phase 4, and it will show a diff and ask
first — a monitor that silently rewrites the configuration of the thing it
monitors has no business calling itself a security tool.

Then:

```sh
agentwatch status
agentwatch events --limit 20
```

## Layout

| Crate | Does |
|---|---|
| `agentwatch-types` | Ids, timestamps, path resolution. No I/O. |
| `agentwatch-events` | The normalized event schema, the adapter trait, the wire format. |
| `agentwatch-storage` | SQLite: migrations, batch writes, queries. |
| `agentwatch-adapter-claude` | Claude Code hook payloads → events. Redaction lives here. |
| `agentwatch-daemon` | Socket server, pipeline, batching. |
| `agentwatch-hook` | The shim Claude Code spawns per tool call. |
| `agentwatch-cli` | `agentwatch`. |

`agentwatch-hook` is a separate crate on purpose: it runs on every tool call, in
the critical path of your own work, so it depends on nothing that would slow its
startup — no Tokio, no SQLite. It opens the socket, writes one frame, and exits.

## Known limits

- Anything the agent does *inside* a `Bash` command is invisible. `cat .env` is
  not a `Read`, so it produces no file event.
- The hook is configured through a file the monitored agent can edit. Fine for
  analytics, disqualifying for anything claiming to be a security control.
- Projects are keyed by working directory, so a session started in a
  subdirectory counts as a separate project from one started at the repo root.
  Phase 3 rolls these up to the repository.
- Cost is not estimated. On a subscription the per-token price is a number
  nobody pays, so tokens are the headline and cost stays opt-in.
