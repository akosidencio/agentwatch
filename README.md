# AgentWatch — local-first monitoring for AI coding agents

**AgentWatch records what your AI coding agents actually do on your machine — sessions, files read and written, shell commands, MCP calls, and token usage — into a local SQLite database. No accounts, no uploads, no outbound telemetry.**

Built in Rust for [Claude Code](https://claude.com/claude-code) and OpenAI Codex on macOS. Everything stays on the box it was collected on.

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Platform: macOS](https://img.shields.io/badge/platform-macOS-lightgrey.svg)](#requirements)
[![Rust 1.90+](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](#build-from-source)
[![Status: v0.1.0](https://img.shields.io/badge/status-v0.1.0-yellow.svg)](#roadmap)

**Keywords:** AI agent monitoring · Claude Code telemetry · OpenAI Codex telemetry · local-first observability · agent audit log · Rust CLI · SQLite

---

## Contents

- [What AgentWatch does](#what-agentwatch-does)
- [What it solves](#what-it-solves)
- [Requirements](#requirements)
- [Install](#install)
- [Updating](#updating)
- [Quick start](#quick-start)
- [Usage](#usage)
- [What it records, and what it never records](#what-it-records-and-what-it-never-records)
- [How it works](#how-it-works)
- [Where it works](#where-it-works)
- [Known limitations](#known-limitations)
- [Roadmap](#roadmap)
- [FAQ](#faq)
- [Build from source](#build-from-source)
- [Uninstall](#uninstall)
- [License](#license)

---

## What AgentWatch does

AgentWatch attaches to Claude Code through its hook system and reads Codex's durable local rollout log, writing both into one normalized event stream at `~/.agentwatch/agentwatch.db`:

| It records | So you can ask |
|---|---|
| Sessions, with repository and git branch | Which projects did agents work on this week? |
| Token usage per response, model, and day | How many tokens did today cost, and on what? |
| Files read and written | What did the agent touch in this repo? |
| Shell commands | What did it run, and when? |
| MCP tool calls | Which MCP servers are actually being used? |
| Access to sensitive paths | Did anything read `.env`, `~/.ssh`, or a credentials file? |

It ships four surfaces over one storage layer: a pipeable CLI (`agentwatch tokens`, `sessions`, `activity`, `security`, `export`), a live full-screen TUI (`agentwatch watch`), a macOS menu bar app, and a JSON Lines export for everything else.

Token totals are duplicate-safe. `agentwatch verify` re-derives them from Claude transcripts and Codex rollouts and reports any drift.

Codex frequently reports a workspace parent as the session `cwd`, even when a
tool is operating in a child repository. AgentWatch also correlates tool
working directories and changed-file paths, keeps every project touched, and
shows the busiest repository as the session's primary project.

### Adapter coverage

| Capability | Claude Code | Codex |
|---|---|---|
| Sessions, surface, model, token usage | Yes | Yes |
| Shell commands | Hook-observed | Recovered from rollout exec calls |
| File writes | Hook-observed | Patch paths, without patch contents |
| File reads | Hook-observed | Not currently available as structured events |
| MCP calls | Hook-observed | Generic tool metadata where reported |
| Prompt handling | Length and SHA-256 only | Prompt is not deserialized |
| Freshness | Immediate hooks, then transcript repair | Startup import and five-minute reconciliation |

## What it solves

**You cannot see what your agents are doing in one place.** Claude Code and Codex each expose their current work differently. Neither gives you a durable cross-agent, cross-project rollup or an answer to "what have these things been doing all week?"

**Token usage is invisible until the bill or the limit arrives.** AgentWatch breaks usage down by repository, model, and calendar day, over all of your history — not just since you installed it.

**Naive token counting is silently wrong.** Claude can repeat one response's usage across several content-block records, while Codex can repeat the same cumulative token snapshot. AgentWatch uses each source's stable response identity and enforces uniqueness in SQLite, so imports and reconcile passes can run repeatedly without moving a total.

**Agents run shell commands you never see.** `agentwatch activity` and `agentwatch security` give you the timeline after the fact, including file access recovered from inside `Bash` commands — labelled `derived`, never mixed in with what a tool actually reported.

**Monitoring tools normally want your data.** This one has no network code. The threat model is your own curiosity and your own audit trail, not a vendor dashboard.

## Requirements

- macOS (Apple Silicon or Intel)
- Claude Code and/or Codex installed
- Nothing else. SQLite is bundled; there is no runtime to install.

Linux, Windows, and other agents (Copilot, Gemini) are not supported yet — see [Roadmap](#roadmap).

### Where it works

| Agent / surface | Monitored | How |
|---|---|---|
| Claude Code CLI | Yes | local hooks plus durable transcripts |
| Claude Code for VS Code | Yes | the same local settings and transcripts; surface reported as `claude-vscode` |
| Claude Code for JetBrains / macOS desktop | Expected | local Claude Code surfaces sharing `~/.claude/settings.json` |
| Codex CLI | Yes | durable rollouts under `$CODEX_HOME/sessions` or `~/.codex/sessions` |
| Codex IDE extension | Yes | the same local rollouts; surface reported by Codex, for example `codex_vscode` |
| Claude Code web and other cloud sessions | No | no hook or durable session log is written to this Mac |
| Agents run over SSH | On the remote host | AgentWatch must run where the agent writes its hooks or rollouts |
| Windows | No | the collector currently uses a Unix socket and launchd |

Claude hooks are read at session start, so a Claude session already running
when you install them remains unmonitored until you open a new one. Codex needs
no hook configuration: the collector imports existing rollouts immediately and
checks for additions every five minutes.

## Install

Install with `curl` from the [binary releases page](https://github.com/akosidencio/agentwatch/releases). This is the only supported installation path — [there is no Homebrew formula and no `cargo install`](#why-curl-is-the-only-supported-install).

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

The archive contains two binaries. `agentwatch` is the whole tool — the CLI, the collector, and the hook are one executable, dispatched by subcommand. `agentwatch-menubar` is the optional status item, and it is separate on purpose: see [why the menu bar is its own binary](#why-the-menu-bar-is-its-own-binary). Both go somewhere on your `PATH`.

Every release publishes a `SHA256SUMS` alongside the archives; the installer verifies against it before writing anything, and you can check by hand with `shasum -a 256 -c SHA256SUMS`. The binaries are unsigned. `curl` does not set the quarantine attribute, so they run as downloaded — but if you fetch them through a browser instead, Gatekeeper will quarantine them and you will have to clear it yourself.

The installer also adds the install directory to your shell profile, unless it is already on your `PATH` (`AGENTWATCH_NO_MODIFY_PATH=1` to skip that).

Then one command sets everything up:

```sh
agentwatch init
```

It registers Claude Code hooks, installs the collector as a LaunchAgent, starts the menu bar, and imports the history already written by Claude Code and Codex. It shows the whole plan — including the exact settings diff — and asks once before writing anything. `--dry-run` shows the plan and writes nothing; `--yes` skips the question; `--no-menu-bar` and `--no-import` leave those steps out.

It is safe to re-run, and worth re-running after an upgrade: steps already done are reported as done, and a launchd job pointing at an older binary is re-pointed at the new one.

For Claude Code, open a **new** session afterwards so the hooks are loaded. Codex rollout collection requires no restart.

Typing `agentwatch` on its own prints the version, whether it is collecting, and the commands worth knowing. `agentwatch --help` is the full list.

### Updating

```sh
agentwatch update                  # the latest release
agentwatch update --version 0.1.2  # a specific one
agentwatch update --dry-run        # show what would change, change nothing
```

It fetches the archive for your architecture, verifies it against the published `SHA256SUMS` *before* unpacking, replaces the binaries by rename, and then **restarts whichever launchd jobs are running**. That last step is the reason this is a command: the plist still points at the right path after a manual re-download, but the running collector holds the inode it started with, so it keeps serving the previous build with nothing to say so.

Hook entries are left alone — they already point at the same path. Stepping backwards is allowed but never silent: it is labelled `DOWNGRADE` and needs the same confirmation.

### Why curl is the only supported install

There is no Homebrew formula, no `cargo install`, no npm package, no Nix flake, and none are planned. Issues and PRs adding them will be declined.

Two reasons. AgentWatch installs a hook into the configuration of the agent it monitors and registers a LaunchAgent; that is not something to hand to a package manager's upgrade cycle running unattended. And every extra channel is another set of binaries to sign, another version to keep in step, and another way for a user to end up on a build the hook config does not match. One channel, one artifact, one version.

If you package it downstream anyway, that is your prerogative — but it is unsupported, and bug reports from third-party packages will be closed.

## Quick start

`agentwatch init` already imported your history, so there is data to look at immediately — Claude transcripts and Codex rollouts, not just work performed since installation:

```sh
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
agentwatch sessions --days 7 --coverage             # counts, surface, and what was observed
agentwatch sessions --agent claude,codex            # one unified session view
agentwatch session latest                           # complete receipt for the newest main-agent session
agentwatch session <id-prefix>                      # receipt for one exact Claude or Codex session
agentwatch compare --by agent --days 30             # compare agent token usage
agentwatch activity --days 1 --kind command,file.write
agentwatch activity --days 7 --project ~/code/myrepo
agentwatch security --days 7                        # access to sensitive paths
agentwatch events --limit 20
agentwatch status
```

`--coverage` is worth knowing about: it reports, per session, what the data *can* answer. `disabled` and `not collected` are distinct from `no` — one means the data was never gathered, the other that it was gathered and there was none.

A session receipt includes its branch, duration, surface, projects, token usage split by model and main/subagent role, files touched, commands, sensitive access, a chronological event timeline, and explicit coverage gaps. Stored command lines are sanitized before they reach the receipt; sensitive entries classify path names without reading file contents.

The receipt is built from AgentWatch's normalized events, so Claude Code and Codex use the same queries and rendering. Claude sidechain usage stays in its parent session; separate Codex subagent rollouts are linked recursively and rolled into the parent receipt. Adapter-specific blind spots remain explicit—for example, Codex rollouts expose patch writes but not direct file-read events, and do not identify every generic tool call as MCP.

Event kinds for `--kind`: `session.started`, `session.ended`, `prompt`, `file.read`, `file.write`, `command`, `mcp.call`, `tool.call`, `token.usage`, `collection.paused`, `collection.resumed`, `config.changed`, `unknown`.

### Live view

```sh
agentwatch watch                                    # full-screen, ~500ms refresh
```

`watch` is a separate surface on purpose. It owns the terminal, so it cannot be piped; every other command prints plain stdout so it can be. Its activity clock uses local time (and labels the UTC fallback), while exportable CLI timelines keep their `utc` label. The screen refreshes every ~500ms, but Codex data advances when its rollout reconciliation runs rather than on every redraw.

The live view stays compact. Press `q`, then run `agentwatch session latest` for the full receipt with commands, files, sensitive access, model/subagent tokens, and timeline.

### Colour

The listings are coloured when they are printed to a terminal and plain when they are not, so `agentwatch sessions | grep` sees exactly the bytes it always did — colour is a reading aid, never part of the output contract. Set `NO_COLOR=1` to turn it off in a terminal, or `CLICOLOR_FORCE=1` to keep it through a pipe into a pager that understands escape sequences.

State is never carried by colour alone: a running daemon is `● running` and a stopped one is `○ not running`, so a stripped or colour-blind reading says the same thing.

### Export and verification

```sh
agentwatch export --days 7 --kind command > commands.jsonl
agentwatch export --days 30 | jq 'select(.kind == "mcp.call")'
agentwatch import                                   # rescan Claude and Codex history safely
agentwatch verify                                   # re-derive totals, report drift
```

### Control

```sh
agentwatch pause                                    # stop recording, reversibly
agentwatch resume
agentwatch service status
agentwatch service install --dry-run                # show the job definition, write nothing
agentwatch service install --menu-bar               # run the status item at login too
agentwatch service status                           # report both jobs
agentwatch install-hooks --dry-run                  # show the diff, write nothing
agentwatch hook-config --binary /path/to/agentwatch # print settings, write nothing
agentwatch init --dry-run                           # show everything setup would do
```

A pause takes effect within a fifth of a second, survives a daemon restart, and never stalls the agent — the daemon still drains connections, it just drops the writes.

### Menu bar (optional)

Live agent state, today's tokens, alert count, and a pause toggle in the macOS menu bar. `agentwatch init` sets it up for you; `agentwatch-menubar` is in the release archive, so it is optional to *run*, not to install:

```sh
agentwatch-menubar &
```

It runs as a macOS *accessory*: no Dock tile, no app-switcher entry, just the status item. The icon is one of three glyphs — a filled aperture while collecting, two bars when paused, a hollow ring when the daemon is not running — beside today's token total, or `paused` / `off` when a bare number would misrepresent why it stopped moving. The menu carries today's counts, a pause toggle, and a shortcut that opens `agentwatch watch` in Terminal.

It reads the database directly every two seconds and redraws only when something changed; it never talks to the daemon, so quitting the icon leaves collection running. It does need to sit in the same directory as `agentwatch`, which is how it finds the binary for the pause and live-view actions.

To keep it across reboots, install it as its own LaunchAgent:

```sh
agentwatch service install --menu-bar
```

It is a separate job from the collector on purpose: a CLI-only user is never handed a menu bar item, and quitting the icon never stops collection. `agentwatch service status` reports both.

It is excluded from the workspace's default members so that building from source stays cheap — `tray-icon` and `winit` add about 35 crates on macOS. From a checkout, build it explicitly:

```sh
cargo build -p agentwatch-menubar --release
```

#### Why the menu bar is its own binary

Everything else is one executable. The menu bar is not, and the reason is measured rather than aesthetic.

`tray-icon` and `winit` pull AppKit and CoreGraphics into whatever binary links them, and dyld loads those frameworks at every launch. The hook is launched by the agent on *every tool call*, so anything linked into it is paid for thousands of times a day. Same machine, same payload, no daemon listening, 150 spawns each:

| binary | size | per hook spawn |
| --- | --- | --- |
| 0.1's standalone `agentwatch-hook` | 367 KB | 7.7 ms |
| `agentwatch` — CLI + collector + hook | 4.6 MB | 9.0 ms |
| the same, with the menu bar folded in | 4.9 MB | 12.8 ms |

The menu bar adds 367 KB and 3.8 ms. Merging everything *except* it costs 1.3 ms and removes two binaries from the archive, which is a trade worth making; adding a status item nobody is looking at to the critical path of every tool call is not. `agentwatch-hook`'s latency test runs against the shipped `agentwatch hook`, so a dependency that changes this answer fails the build rather than quietly slowing every tool call down.

### Which surface a session ran in

`agentwatch sessions` shows agent, model, dominant repository, and a `surface`
column carrying the source's own value — `claude-vscode` or `codex_vscode`, for example. It is
stored verbatim rather than mapped onto an enum of ours, so a value we have
never seen shows up as itself instead of collapsing into `Other`.

Only durable logs carry the field, so a session seen purely through hooks reads
as `?` until it is reconciled. That is *unknown*, not a default.

## What it records, and what it never records

Never stored, by construction:

- **Prompt text.** Claude prompts become a character count and SHA-256 digest; Codex prompts are not deserialized.
- **Tool output.** `tool_response` is never deserialized at all, so file contents and command output never reach a Rust value, let alone the database.
- **File contents** from `Write` and `Edit` payloads.

Shell command lines **are** recorded because they are part of the audit trail. Before storage, AgentWatch redacts common token, password, API-key, authorization-header, and URL-credential forms. This is a safety net, not a shell-language secret scanner; unusual or dynamically constructed secrets may still evade it.

Nothing leaves your machine. There is no network client in the codebase.

## How it works

```
Claude Code hooks ──► unix socket ──┐
                                    ├─► normalize ──► redact ──► SQLite (WAL)
Codex rollout JSONL ─► reconcile ───┘          │
                                          AgentEvent
                                       + EvidenceSource
```

The Claude hook runs on every tool call, in the critical path of your own work, so the code it runs touches nothing that would slow its startup — no Tokio, no SQLite, no terminal. It opens the socket, writes one frame, and exits. Measured round trip: **~9ms** (n=150, release build; 7.7ms for 0.1's standalone binary — the difference is process spawn on a larger image, and is why [the menu bar stays separate](#why-the-menu-bar-is-its-own-binary)). **The hook exits 0 on every path**, including "daemon not running", malformed input, and oversized payloads. It cannot fail one of your tool calls. Codex does not spawn an AgentWatch hook; its durable rollout is reconciled out of band.

| Crate | Does |
|---|---|
| `agentwatch-types` | Ids, timestamps, path resolution. No I/O. |
| `agentwatch-events` | The normalized event schema, the adapter trait, the wire format. |
| `agentwatch-storage` | SQLite: migrations, batch writes, queries. |
| `agentwatch-adapter-claude` | Claude Code hook payloads and transcripts → events. |
| `agentwatch-adapter-codex` | Codex rollout metadata → sessions, commands, file writes, models, and token usage. |
| `agentwatch-daemon` | Socket server, pipeline, batching, reconciliation. Runs as `agentwatch daemon`. |
| `agentwatch-hook` | The shim Claude Code spawns per tool call. Runs as `agentwatch hook`. |
| `agentwatch-cli` | The `agentwatch` binary. Every command, and the two above. |
| `agentwatch-menubar` | The macOS tray app, and the one separate binary. |

The crates stayed separate when the binaries merged: the hook still cannot reach the database or the runtime, because its crate does not depend on them. That boundary is the thing worth keeping, not the number of files in the archive.

`init` and `install-hooks` add their entries in their own matcher group, so hooks belonging to other tools are never modified and an uninstall removes exactly what was added. Entries written by an older version are recognised and *repointed* rather than duplicated, which is what makes an upgrade safe: the alternative is a settings file pointing at a binary that no longer exists, and a hook that cannot run reports nothing without saying so. Every write shows a diff first and keeps a timestamped backup. This is verified in the test suite against a real settings file carrying another tool's hooks: install → uninstall restores it byte for byte, key order included.

## Known limitations

These are design consequences, stated up front rather than discovered later.

- **Anything Claude does inside a `Bash` command is opaque to its hooks.** `cat .env` is not a `Read`, so AgentWatch records the command and derives notable path references from it, but cannot claim a tool-observed file read.
- **The Claude hook is configured through a file the monitored agent can edit.** Fine for analytics, disqualifying for anything claiming to be a security control.
- **Sensitive paths are classified by name, never by reading them.** A credential in an ordinarily-named file is invisible. Reading files to find out would mean AgentWatch handling every secret on the machine, which is a worse position than the one it reports on.
- **Command lines are scanned, not parsed.** `cat .env` is caught; `eval`, variable expansion, and heredocs are not. Anything recovered this way is labelled `derived` and never mixed in with tool-reported file events.
- **Command redaction is intentionally conservative, not complete.** Common credential forms are removed before storage, but shell expansion, heredocs, unusual option names, and secrets embedded in arbitrary text can evade it.
- **Collection can be paused, and Claude hook collection can be switched off by editing its settings.** Neither is prevented — nothing running as your own user could prevent it. Both are *recorded*: a pause writes `collection.paused`, and Claude hooks disappearing from the settings file writes `config.changed`. A gap in the timeline should say why it is there rather than looking like an idle agent.
- **Claude Code web (claude.ai/code) is not monitored, and cannot be.** The agent runs on Anthropic's infrastructure, so there is no local process to hook, no local socket to reach, and no transcript written to your disk. The same applies to cloud and remote sessions. A quiet day in `agentwatch tokens` may only mean you worked in the browser.
- **`claude` run over SSH monitors the remote machine, not yours.** Hooks and transcripts land on whichever host the agent runs on.
- **Windows is not supported.** The daemon uses a Unix domain socket and the service uses launchd.
- **Cost is not estimated.** On a subscription the per-token price is a number nobody pays, so tokens are the headline and cost stays opt-in until it can be labelled honestly.
- **Attempted-but-denied tool calls are not recorded.** `PreToolUse` needs a real captured payload to confirm a pre-hook can be correlated to its post-hook; installing both without that would double every tool call's rows for no gain.
- **Claude transcripts and Codex rollouts are undocumented formats.** Readers ignore unknown records and deserialize metadata selectively, but a breaking source-format change may need an AgentWatch release. `agentwatch verify` is the drift detector.
- **Codex is near-live, not hook-live.** Rollouts are reconciled at collector startup and every five minutes. Claude hook events normally appear immediately.
- **Retention is unbounded.** Nothing expires or vacuums yet.

## Roadmap

### Next — planned features

- [ ] **`PreToolUse` correlation** — record what an agent *tried* to do and was denied, not only what completed. Blocked until a real session confirms a pre-hook can be correlated to its post-hook; installing both without that would double every tool call's rows.
- [ ] **A third agent adapter (Gemini)** — the adapter trait has existed since phase 1 for exactly this. Agent #2 is where the product starts; agent #1 proved the pipeline.
- [ ] **Session diff and summary** — what changed in a repository over a session, from the file events already recorded.
- [ ] **Alerting on sensitive access** — a notification when something reads a credential path, rather than finding it in `agentwatch security` a week later.

### Later — wanted, not scheduled

- [ ] **Opt-in cost estimation** — labelled as API-equivalent, because on a subscription the per-token price is a number nobody actually pays.
- [ ] **Retention and vacuum** — needs data worth expiring first.
- [ ] **OTLP receiver** — would remove the per-tool-call process spawn entirely. Worth evaluating once it is clear what the hooks miss.
- [ ] **Linux and Windows** — macOS first, because that is where this is dogfooded daily.
- [ ] **Policy engine / rules file** — there is nothing to enforce yet.
- [ ] **Web dashboard** — largest cost in the original spec, least informative about demand. The CLI answers the same question and the TUI covers the live case for a fraction of the cost.


## FAQ

**Does AgentWatch send anything anywhere?**
No. There is no network client in the codebase, and no telemetry, analytics, or update check of any kind. Data lives in `~/.agentwatch/agentwatch.db` and nowhere else.

The one time anything leaves the machine is when you type `agentwatch update`, which shells out to `curl` to fetch a release archive from GitHub — a download, not an upload, and only on request. Keeping the transport out of the binary is deliberate: it is what makes the sentence above checkable with `grep`.

**Will it slow down my agent?**
For Claude Code, the hook adds a measured ~9ms per tool call, and that cost is process spawn rather than anything AgentWatch computes — which is also why [the menu bar is a separate binary](#why-the-menu-bar-is-its-own-binary). It exits 0 on every path, so it cannot fail a tool call even when the daemon is down. Codex uses no per-tool AgentWatch hook; rollout parsing happens in the collector.

**Can it read my prompts or my code?**
No. Claude prompts are reduced to a character count and hash, Codex prompts are skipped, and tool responses and patch bodies are never deserialized. Shell command lines are retained for the audit trail, with common token, password, API-key, authorization-header, and URL-credential forms redacted before storage.

**Does it work with Cursor, Codex, Copilot, or Gemini?**
Claude Code and Codex are supported. Copilot, Cursor, and Gemini do not have adapters yet.

**Why doesn't it show me a dollar cost?**
Because on a subscription the per-token price is a number nobody actually pays, and a confident wrong number is worse than no number.

**Is it a security tool?**
It is an audit trail, not a control. It observes and records; it does not block, and it cannot defend against an agent that edits the settings file it is configured from. It records that too.

## Build from source

For development, or if you would rather not run a binary you did not compile. The [installer](#install) is the supported path for everyone else.

```sh
git clone https://github.com/akosidencio/agentwatch
cd agentwatch
cargo build --release
cargo test

# The menu bar is outside the workspace's default members. Skip this line and
# `init` reports the menu bar as unavailable rather than failing.
cargo build --release -p agentwatch-menubar

mkdir -p ~/.local/bin
cp target/release/agentwatch target/release/agentwatch-menubar ~/.local/bin/

agentwatch init
```

`init` wires everything to the binary that is running it, so a checkout can set itself up without touching an installed copy — and re-running the installed one later points everything back.

## Uninstall

One command, the mirror image of [`init`](#install):

```sh
agentwatch uninstall
```

It removes the hooks, stops and deletes both launchd jobs, takes the installer's line back out of your shell profile, and deletes the executables — showing the whole plan, including the settings diff, and asking once.

**Your collected data is kept unless you ask for it to go.** Binaries can be downloaded again; months of history cannot. Re-installing picks up where it left off.

```sh
agentwatch uninstall --dry-run         # show what would be removed, remove nothing
agentwatch uninstall --purge           # also delete the database and everything in it
agentwatch uninstall --keep-binaries   # unhook everything, leave the executables
```

The hook removal takes out exactly what was added — including entries written by 0.1 — and leaves any other tool's hooks untouched. The shell profile edit is equally narrow: only a block carrying the installer's `# added by the AgentWatch installer` marker is touched, and only the one line under it.

It removes the binary it is *running from*, the same way `init` wires up the binary it is running from, so run the installed copy rather than one in a build tree. The plan names the directory before you agree to it.

The individual commands still exist, if you would rather do it a piece at a time:

```sh
agentwatch install-hooks --uninstall
agentwatch service uninstall
agentwatch service uninstall --menu-bar
```

## License

Dual licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
