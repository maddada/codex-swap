# Automatic handoff in cmux

`codex-auto` is an optional Python launcher for interactive Codex on macOS and
Linux. It uses `xswap run`, separate account homes, shared conversation history,
and Codex's `--no-daemon` mode. Other panes and Codex processes keep running.
It never uses a global account switch or a process-name-based kill command.

## Install

Install the official Codex CLI and xswap first. Save at least two accounts with
`xswap add --login --alias NAME`. Python 3.9 or newer is required; Python 3.11+
also supports arbitrary TOML syntax for custom `sqlite_home` configuration.

From this checkout:

```sh
mkdir -p "$HOME/.local/bin" "$HOME/.config/codex-auto"
install -m 755 scripts/codex-auto scripts/codex-auto-cli "$HOME/.local/bin/"
install -m 644 scripts/codex-auto.zsh "$HOME/.config/codex-auto/cmux.zsh"
```

Ensure `~/.local/bin` is on PATH. Add this line to `~/.zshrc`:

```sh
source "$HOME/.config/codex-auto/cmux.zsh"
```

New cmux shells now route `codex`, `codex resume`, and `codex fork` through the
launcher. In an existing idle shell, source that file once. Existing running
sessions are not adopted or restarted. Outside cmux, `codex` is unchanged.
`command codex` bypasses the shell function.

The integration preserves cmux's native Codex wrapper and its per-invocation
hooks and computer-use setup. xswap always launches the actual Codex executable,
so subprocesses cannot recursively enter the account wrapper. Non-interactive
commands such as `codex exec`, `codex login`, `codex --help`, and `codex --version`
pass directly to the official CLI. Remote and ephemeral sessions also pass through.

## Configure

Create `~/.config/codex-auto/config.json`, for example:

```json
{
  "accounts": ["primary", "secondary", "work"],
  "account": "primary",
  "threshold": 97,
  "poll_seconds": 30,
  "full_permissions": false,
  "real_codex": "/absolute/path/to/the/official/codex"
}
```

Aliases must match enabled, saved xswap accounts. Without configuration, the
launcher uses enabled accounts in registry order, starting with the active
account when it is in that pool. `real_codex` should reference a stable official
launcher, never a cmux shim or `codex-auto-cli`. It can also be supplied through
`CODEX_AUTO_REAL_CODEX`. `CODEX_AUTO_CONFIG` selects a different config file.

Set `full_permissions` to `true` to explicitly run every managed launch and
handoff with `--dangerously-bypass-approvals-and-sandbox`. In that mode the launcher
removes narrower CLI sandbox/approval flags before starting Codex. It does not
add a hook-trust bypass; existing cmux invocation flags are retained.

You can also run the wrapper directly:

```sh
codex-auto --check
codex-auto --threshold 97 --account primary
codex-auto --full-permissions -- -m MODEL 'your task'
codex-auto --session EXACT_SESSION_UUID
```

## What triggers a handoff

Core Codex quota windows are checked in the background every 30 seconds. Fresh
`token_count` records from the managed conversation also update usage. At 97%
used in any core window, a handoff is scheduled for a completed-turn boundary or
an idle conversation. A partially typed composer draft delays an idle handoff.
A long active turn can take usage above 97% before it finishes.

Only accounts with confirmed available quota below the threshold are selected.
When no alternative is available, the current session can keep using its
remaining quota. After an actual `usage_limit_exceeded` failure, only that
launch's private PTY process group is stopped and the exact conversation is
resumed on another account. An unfinished ordinary request gets a continuation
prompt that asks Codex to inspect state and avoid duplicating work. Original
prompts are never replayed. Generic HTTP 429 errors, transient request limits,
and failed quota reads do not by themselves cause rotation.

If every account is exhausted, at the threshold, or unavailable after an actual
quota failure, the wrapper waits and checks again. A rejected account has a
five-minute cooldown before it can be retried. Exiting Codex normally exits the
wrapper; Ctrl-C exits while the wrapper is waiting for quota.

## Chats and goals

The launcher follows `/new`, `/resume`, and `/fork` in the TUI using its displayed
session-ID terminal-title item and the exact IDs in its private process log.
Codex currently abbreviates IDs in terminal titles; a prefix must resolve to
exactly one ID from this invocation or proactive handoff is withheld. Arbitrary
UUIDs printed in chat text are ignored. Background subagent logs do not change
the selected conversation. Keep the `session-id` terminal-title item enabled
while using the wrapper.

History and SQLite state, including goal identity, objective, budget and usage,
are shared through xswap. Active goals use Codex's native continuation after
resume, without an extra continuation prompt. A `usage_limited` goal is resumed
with the native `/goal resume` command after the resumed CLI becomes ready.
Explicitly paused, blocked, complete, and budget-limited goals are not restarted.
The launcher reads goal state but never edits its database or recreates a goal.

Resuming across accounts sends the selected conversation's context using the
next account. Per-account billing and permissions still apply. The launcher does
not buy credits or alter billing settings. This manages the focused conversation;
it does not migrate all background subagent runtimes or other cmux panes.

## Recovery and verification

Private logs and per-launch recovery JSON are stored under
`~/.local/state/codex-auto`. The JSON records the exact session UUID and last
account; use `codex-auto --session UUID` after resolving a login or connectivity
problem. The launcher never chooses a conversation with `--last` during handoff.

Run the isolated tests without making model requests:

```sh
python3 -m unittest discover -s tests/auto_swap -v
zsh -n scripts/codex-auto.zsh
sh -n scripts/codex-auto-cli
```

Tests use fake xswap/CLI processes in real PTYs and temporary SQLite fixtures.
They exercise proactive and error-triggered rotation, exact conversation
selection, full permissions, goal status/accounting preservation, and survival
of an unrelated process. Recheck compatibility after major Codex CLI updates;
the launcher depends on its session history, terminal-title and log formats.
