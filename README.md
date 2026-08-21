# AgentWatch — local-first monitoring for AI coding agents

**AgentWatch records what your AI coding agent actually does on your machine — sessions, files read and written, shell commands, MCP calls, and token usage — into a local SQLite database. No accounts, no telemetry, no network calls.**

Built in Rust for [Claude Code](https://claude.com/claude-code) on macOS. Everything stays on the box it was collected on.

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Platform: macOS](https://img.shields.io/badge/platform-macOS-lightgrey.svg)](#requirements)
[![Rust 1.90+](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](#build-from-source)
[![Status: v0.1.0](https://img.shields.io/badge/status-v0.1.0-yellow.svg)](#roadmap)

**Keywords:** AI agent monitoring · Claude Code token usage · local-first observability · agent audit log · developer telemetry without telemetry · Rust CLI · SQLite

---

## Contents

- [What AgentWatch does](#what-agentwatch-does)
- [What it solves](#what-it-solves)
- [Requirements](#requirements)
- [Install](#install)
- [Quick start](#quick-start)
- [Usage](#usage)
- [What it records, and what it never records](#what-it-records-and-what-it-never-records)
- [How it works](#how-it-works)
- [Known limitations](#known-limitations)
- [Roadmap](#roadmap)
- [FAQ](#faq)
- [Build from source](#build-from-source)
- [Uninstall](#uninstall)
- [License](#license)

---

## What AgentWatch does

AgentWatch attaches to Claude Code through its hook system and writes a normalized event stream to `~/.agentwatch/agentwatch.db`:

| It records | So you can ask |
|---|---|
| Sessions, with repository and git branch | Which projects did agents work on this week? |
| Token usage per response, model, and day | How many tokens did today cost, and on what? |
| Files read and written | What did the agent touch in this repo? |
| Shell commands | What did it run, and when? |
| MCP tool calls | Which MCP servers are actually being used? |
| Access to sensitive paths | Did anything read `.env`, `~/.ssh`, or a credentials file? |

It ships four surfaces over one storage layer: a pipeable CLI (`agentwatch tokens`, `sessions`, `activity`, `security`, `export`), a live full-screen TUI (`agentwatch watch`), a macOS menu bar app, and a JSON Lines export for everything else.

Token totals are exact. `agentwatch verify` re-derives them from Claude Code's own transcripts and reports any drift — currently **0 drift across 149 transcripts and 5,998,457,883 tokens**.

## What it solves

**You cannot see what your agents are doing.** Claude Code shows you the current session and nothing else. There is no history, no cross-project rollup, no answer to "what has this thing been doing all week."

**Token usage is invisible until the bill or the limit arrives.** AgentWatch breaks usage down by repository, model, and calendar day, over all of your history — not just since you installed it.

**Naive token counting is silently wrong.** One API response is written to the transcript as *several* records, one per content block, each repeating the whole response's usage. Counting records instead of responses inflates totals by **1.92x** on the real corpus here (3.08x on input tokens alone). AgentWatch deduplicates by `message.id` and enforces it with a unique index, so a reconcile pass can run any number of times without a total moving.

**Agents run shell commands you never see.** `agentwatch activity` and `agentwatch security` give you the timeline after the fact, including file access recovered from inside `Bash` commands — labelled `derived`, never mixed in with what a tool actually reported.

**Monitoring tools normally want your data.** This one has no network code. The threat model is your own curiosity and your own audit trail, not a vendor dashboard.

## Requirements

- macOS (Apple Silicon or Intel)
- Claude Code installed
- Nothing else. SQLite is bundled; there is no runtime to install.

Linux, Windows, and other agents (Codex, Gemini) are not supported yet — see [Roadmap](#roadmap).

## Install

> **Not published yet.** The commands below are the *only* installation path AgentWatch will support, but the first tagged release has not been cut, so the URLs 404 today. Until then, [build from source](#build-from-source). Everything after the download step is already implemented and works.

Install with `curl` from the binary releases page:

```sh
curl -fsSL https://github.com/akosidencio/agentwatch/releases/latest/download/install.sh | sh
```

Or download and extract the archive yourself, if you would rather not pipe a script into a shell:

```sh
# Apple Silicon
curl -fsSL https://github.com/akosidencio/agentwatch/releases/latest/download/agentwatch-aarch64-apple-darwin.tar.gz \
  | tar xz -C ~/.local/bin

# Intel
curl -fsSL https://github.com/akosidencio/agentwatch/releases/latest/download/agentwatch-x86_64-apple-darwin.tar.gz \
  | tar xz -C ~/.local/bin
```

The archive contains three binaries — `agentwatch`, `agentwatch-daemon`, and `agentwatch-hook` — and they must all land somewhere on your `PATH`.

Then wire it up:

```sh
agentwatch install-hooks     # shows the exact settings diff and asks first
agentwatch service install   # runs the daemon at login
```

Hooks are read at session start, so open a **new** agent session afterwards. An already-running session is not monitored.

### Why curl is the only supported install

There is no Homebrew formula, no `cargo install`, no npm package, no Nix flake, and none are planned. Issues and PRs adding them will be declined.

Two reasons. AgentWatch installs a hook into the configuration of the agent it monitors and registers a LaunchAgent; that is not something to hand to a package manager's upgrade cycle running unattended. And every extra channel is another set of binaries to sign, another version to keep in step, and another way for a user to end up on a build the hook config does not match. One channel, one artifact, one version.

If you package it downstream anyway, that is your prerogative — but it is unsupported, and bug reports from third-party packages will be closed.

## Quick start

Import your history first. This reads every transcript Claude Code has already written, so the tool is useful on the day you install it rather than after a week of collecting:

```sh
agentwatch import                          # safe to re-run; nothing is double counted
agentwatch tokens --all --by project       # where your tokens have gone, all time
```

Then the daily loop:

```sh
agentwatch tokens                          # today, broken down by repository
agentwatch watch                           # live view, while an agent is working
```

## Usage

### Token usage

```sh
agentwatch tokens                                   # today, by repository
agentwatch tokens --days 7 --by day
agentwatch tokens --all --by project --limit 10     # rolled up to repositories
agentwatch tokens --all --by directory              # exact working directories
agentwatch tokens --from 2026-08-01 --to 2026-08-21 --by model
```

Four counters are tracked separately — input, output, cache creation, and cache read — because providers bill them very differently and merging them at ingestion would make cost estimation permanently unfixable.

### Sessions and activity

```sh
agentwatch sessions --days 7 --coverage             # counts, plus what was observed
agentwatch activity --days 1 --kind command,file.write
agentwatch activity --days 7 --project ~/code/myrepo
agentwatch security --days 7                        # access to sensitive paths
agentwatch events --limit 20
agentwatch status
```

`--coverage` is worth knowing about: it reports, per session, what the data *can* answer. `disabled` and `not collected` are distinct from `no` — one means the data was never gathered, the other that it was gathered and there was none.

Event kinds for `--kind`: `session.started`, `session.ended`, `prompt`, `file.read`, `file.write`, `command`, `mcp.call`, `tool.call`, `token.usage`, `collection.paused`, `collection.resumed`, `config.changed`, `unknown`.

### Live view

```sh
agentwatch watch                                    # full-screen, ~500ms refresh
```

`watch` is a separate surface on purpose. It owns the terminal, so it cannot be piped; every other command prints plain stdout so it can be.

### Export and verification

```sh
agentwatch export --days 7 --kind command > commands.jsonl
agentwatch export --days 30 | jq 'select(.kind == "mcp.call")'
agentwatch verify                                   # re-derive totals, report drift
```

### Control

```sh
agentwatch pause                                    # stop recording, reversibly
agentwatch resume
agentwatch service status
agentwatch service install --dry-run                # show the job definition, write nothing
agentwatch install-hooks --dry-run                  # show the diff, write nothing
agentwatch hook-config --binary /path/to/agentwatch-hook   # print settings, write nothing
```

A pause takes effect within a fifth of a second, survives a daemon restart, and never stalls the agent — the daemon still drains connections, it just drops the writes.

### Menu bar (optional)

Live agent state, today's tokens, alert count, and a pause toggle in the macOS menu bar. It is built and shipped separately on purpose: `tray-icon` and `winit` add about 35 crates on macOS, which is not a cost to impose on people who only use the CLI.

```sh
cargo build -p agentwatch-menubar --release
```

## What it records, and what it never records

Never stored, by construction:

- **Prompt text.** Only a character count and a SHA-256 digest, so repeated prompts can be recognized as repeats without anyone being able to read what they said.
- **Tool output.** `tool_response` is never deserialized at all, so file contents and command output never reach a Rust value, let alone the database.
- **File contents** from `Write` and `Edit` payloads.

Shell command lines **are** recorded, because a command monitor that hides commands is pointless. That means a command containing a secret gets stored. Scrubbing for the obvious shapes is not implemented yet — see [Known limitations](#known-limitations).

Nothing leaves your machine. There is no network client in the codebase.

## How it works

```
claude code
    │  SessionStart / UserPromptSubmit / PostToolUse / SessionEnd
    ▼
agentwatch-hook           spawn → write one frame → exit
    │
    │  unix socket, u32-LE length prefix + JSON envelope
    ▼
daemon: ingest ──► mpsc(bounded) ──► normalize ──► batch ──► sqlite (WAL)
                                          │
                                     redaction
                                          │
                                    AgentEvent + EvidenceSource
```

The hook runs on every tool call, in the critical path of your own work, so it depends on nothing that would slow its startup — no Tokio, no SQLite. It opens the socket, writes one frame, and exits. Measured round trip: **median 6.97ms, p95 8.27ms** (n=200, release build). The cost is process spawn, not our code. **The hook exits 0 on every path**, including "daemon not running", malformed input, and oversized payloads. It cannot fail one of your tool calls.

| Crate | Does |
|---|---|
| `agentwatch-types` | Ids, timestamps, path resolution. No I/O. |
| `agentwatch-events` | The normalized event schema, the adapter trait, the wire format. |
| `agentwatch-storage` | SQLite: migrations, batch writes, queries. |
| `agentwatch-adapter-claude` | Claude Code hook payloads → events. Redaction lives here. |
| `agentwatch-daemon` | Socket server, pipeline, batching, reconciliation. |
| `agentwatch-hook` | The shim Claude Code spawns per tool call. |
| `agentwatch-cli` | `agentwatch`. |
| `agentwatch-menubar` | The macOS tray app. Opt-in build. |

`install-hooks` adds its entries in their own matcher group, so hooks belonging to other tools are never modified and an uninstall removes exactly what was added. Every write shows a diff first and keeps a timestamped backup. This is verified in the test suite against a real settings file carrying another tool's hooks: install → uninstall restores it byte for byte, key order included.

## Known limitations

These are design consequences, stated up front rather than discovered later.

- **Anything the agent does inside a `Bash` command is invisible to the hooks.** `cat .env` is not a `Read`, so it produces no file event.
- **The hook is configured through a file the monitored agent can edit.** Fine for analytics, disqualifying for anything claiming to be a security control.
- **Sensitive paths are classified by name, never by reading them.** A credential in an ordinarily-named file is invisible. Reading files to find out would mean AgentWatch handling every secret on the machine, which is a worse position than the one it reports on.
- **Command lines are scanned, not parsed.** `cat .env` is caught; `eval`, variable expansion, and heredocs are not. Anything recovered this way is labelled `derived` and never mixed in with tool-reported file events.
- **Secrets inside command lines are stored unscrubbed.** Scrubbing for the obvious shapes is not implemented.
- **Collection can be paused, and can be switched off entirely by editing the agent's settings.** Neither is prevented — nothing running as your own user could prevent it. Both are *recorded*: a pause writes `collection.paused`, and hooks disappearing from the settings file writes `config.changed`. A gap in the timeline should say why it is there rather than looking like an idle agent.
- **Cost is not estimated.** On a subscription the per-token price is a number nobody pays, so tokens are the headline and cost stays opt-in until it can be labelled honestly.
- **Attempted-but-denied tool calls are not recorded.** `PreToolUse` needs a real captured payload to confirm a pre-hook can be correlated to its post-hook; installing both without that would double every tool call's rows for no gain.
- **Claude Code's transcript format is undocumented.** AgentWatch is pinned to it, and a format change will need a release. `agentwatch verify` is how you find out.
- **Retention is unbounded.** Nothing expires or vacuums yet.
- **The menu bar icon has not been visually confirmed.** It builds, its formatting logic is unit tested, and it runs without error, but seeing the icon appear needs a GUI session.

## Roadmap

**v0.1.0 — shipped.** Claude Code, macOS, local only. Phases 1–4 complete: the hook → daemon → SQLite spine, exact token reconciliation, the CLI and live TUI, and the menu bar plus safe hook and service installation. 267 tests, clippy clean.

**Next up**

| | Why it's next |
|---|---|
| **Binary releases + `curl` installer** | The install path documented above is the whole distribution story, and it does not exist yet. Highest priority. |
| **`PreToolUse` correlation** | Recording what an agent *tried* to do and was denied. Now unblockable: with hooks installable, a real session produces the payloads needed to confirm a correlation id exists. |
| **Secret scrubbing in command lines** | The one place where AgentWatch can store something you did not intend to keep. |
| **Agent #2 (Codex, Gemini)** | The adapter trait has existed since phase 1 for exactly this. Agent #2 is where the product starts; agent #1 proved the pipeline. |

**Later, deliberately deferred**

| Deferred | Why |
|---|---|
| Full web dashboard | Largest cost, least informative about demand. The CLI answers the same question and the TUI covers the live case for a fraction of the cost. |
| Cost estimation | Misleading for subscription users. Opt-in later, labelled as API-equivalent. |
| Network monitoring | Not attributable to a PID without a Network Extension. Cannot be delivered honestly. |
| Process tree | Polling misses short-lived children and fights the idle-CPU target. |
| Policy engine / rules | Nothing to enforce yet. |
| OTLP receiver | Would remove the per-tool-call process spawn. Evaluate once it is clear what the hooks miss. |
| Retention and vacuum | Needs data worth expiring first. |
| Linux and Windows | macOS first, because that is where this is dogfooded daily. |

## FAQ

**Does AgentWatch send anything anywhere?**
No. There is no network client in the codebase. Data lives in `~/.agentwatch/agentwatch.db` and nowhere else.

**Will it slow down my agent?**
The hook adds a measured ~7ms median per tool call, and that cost is process spawn rather than anything AgentWatch computes. It exits 0 on every path, so it cannot fail a tool call even when the daemon is down.

**Can it read my prompts or my code?**
No. Prompts are reduced to a character count and a hash at the adapter boundary, and tool responses are never deserialized. Shell command lines are stored in full, deliberately.

**Can I install it with Homebrew or `cargo install`?**
No, and that is not an oversight — see [why curl is the only supported install](#why-curl-is-the-only-supported-install).

**Does it work with Cursor, Codex, Copilot, or Gemini?**
Not yet. Claude Code only. The adapter trait exists so agent #2 is a week of work rather than a rewrite.

**Why doesn't it show me a dollar cost?**
Because on a subscription the per-token price is a number nobody actually pays, and a confident wrong number is worse than no number.

**Is it a security tool?**
It is an audit trail, not a control. It observes and records; it does not block, and it cannot defend against an agent that edits the settings file it is configured from. It records that too.

## Build from source

Needed until the first binary release exists.

```sh
git clone https://github.com/akosidencio/agentwatch
cd agentwatch
cargo build --release
cargo test

mkdir -p ~/.local/bin
cp target/release/agentwatch target/release/agentwatch-daemon target/release/agentwatch-hook ~/.local/bin/

agentwatch install-hooks
agentwatch service install
```

Requires Rust 1.90+ (edition 2024). `unsafe_code` is forbidden workspace-wide.

Running the daemon in the foreground instead of as a service, for development:

```sh
./target/release/agentwatch-daemon
```

## Uninstall

```sh
agentwatch install-hooks --uninstall
agentwatch service uninstall
rm -rf ~/.agentwatch          # deletes collected data
rm ~/.local/bin/agentwatch ~/.local/bin/agentwatch-daemon ~/.local/bin/agentwatch-hook
```

The hook uninstall removes exactly what was added and leaves any other tool's hooks untouched.

## License

Dual licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
