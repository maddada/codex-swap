# Automatic handoff in cmux

`codex-auto` is an optional Python launcher for interactive Codex on macOS and
Linux. It uses `xswap run`, separate account homes, shared conversation history,
and Codex's `--no-daemon` mode. Other panes and Codex processes keep running.
It never uses a global account switch or a process-name-based kill command.

## Install

Install the official Codex CLI (tested with 0.166.0) and xswap first. Save at least two accounts with
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
  "cooldown_seconds": 300,
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

### Change settings without editing JSON

```sh
codex-auto configure --show
codex-auto configure --remaining-percent 3
codex-auto configure --threshold 95 --poll-seconds 15 --cooldown-seconds 120
codex-auto configure --accounts primary,work,secondary --account primary
codex-auto configure --full-permissions
codex-auto configure --no-full-permissions
```

`--threshold` means percentage **used**; `--remaining-percent` means percentage
**left**. Choose one: 97 used and 3 remaining are equivalent. The supported
cutoff is greater than 0 and at most 100 used (0 to less than 100 remaining).
The cutoff still waits for the same safe native checkpoint; it never forces an
immediate interruption at an exact usage percentage.

`poll_seconds` controls background quota checks (minimum 0.1 seconds).
`cooldown_seconds` controls how long a rejected account is excluded before
retry (minimum 0; default 300). Polling more frequently makes more usage queries.
Account order controls fallback preference; aliases must already be saved and
enabled in xswap before launching. Configuration editing validates the pool's
shape without starting Codex or contacting accounts.

Only specified settings change. Unrelated keys, the real Codex path and existing
permission choices are preserved. Configuration edits serialize through a stable owner-private lock file and reread
the latest settings while holding it, preserving concurrent edits to different
keys. Writes replace the configuration atomically
with owner-only permissions; `--show` displays supported effective settings
without exposing unrelated configuration values or writing a file.
`CODEX_AUTO_CONFIG` also selects the file edited by this command.

Settings apply to **future wrapper launches**, including ordinary `codex` in
new cmux shells configured as above. Running sessions retain their existing
settings. One-off `--threshold` and `--poll-seconds` launcher flags override saved
values for that invocation. Use `configure --show` to inspect saved defaults and
`--check` to inspect current account quota.

## What triggers a handoff

Core Codex quota windows are checked in the background every 30 seconds. Fresh
`token_count` records also update usage. At 97%, the wrapper arms a private gate
for the focused conversation. It does **not** infer a safe restart from a
completed-turn event or an idle-looking terminal: a goal can start another turn
before the launcher processes either signal.

The launcher adds a synchronous `PreModelRequest` hook while preserving existing
inline handlers through Codex's effective `config/read` result. The callback
parks before a model request can start and acknowledges its exact session,
turn ID and one-use gate nonce. Handoff requires that turn's start to be present
in the persisted transcript and the callback's lease to still be live. The
launcher then freezes and terminates only its own PTY group, so the callback's
exit cannot release another request during shutdown. A stale, expired, absent
or untrusted callback never causes an automatic restart.

Handoff happens at a pre-request checkpoint, which can be within a turn after
its tools finish. An ordinary idle chat waits for its next request; a running
goal checkpoints at its next model request. A long model request or tool call
can take usage above 97% before the callback is reached. Unsubmitted text,
including multiline bracketed pastes, defers handoff. Submitted input also defers
handoff until each submitted prompt has an exact matching persisted user-message
receipt and the PTY input queue is empty. An unrelated message or automatic goal
turn is not a receipt. Terminal replies, empty Enter and cancelled drafts do not
create pending message receipts. Basic typing, bracketed pastes and backspace are tracked; ambiguous draft-control keys, unsupported composer edits or native commands that do not
persist a matching message conservatively defer automatic handoff for that
invocation. Resume manually when needed; there is no timeout that discards
unacknowledged input. Existing custom hook
handlers should tolerate cancellation and retries during a handoff.

The native hook must be supported, enabled and trusted. Review it using Codex's
normal hook trust UI if prompted; the launcher never adds a hook-trust bypass.
If policy disables the callback, the CLI keeps running but automatic rotation
waits for a valid acknowledgement instead of stopping the process from logs.
A failed config preflight stops the launcher before starting a managed session.

Only accounts with confirmed available quota below the threshold are selected.
When no alternative is available, the current session can keep using its
remaining quota. After an actual `usage_limit_exceeded` failure, the wrapper also waits for
a cooperative checkpoint before resuming the exact conversation on another
account. A stalled ordinary request may receive an automatic continuation to
reach that checkpoint; a quota-paused goal uses `/goal resume`. An unfinished ordinary request gets a continuation
prompt that asks Codex to inspect state and avoid duplicating work. Original
prompts are never replayed. Generic HTTP 429 errors, transient request limits,
and failed quota reads do not by themselves cause rotation.

If every account is exhausted, at the threshold, or unavailable after an actual
quota failure, the wrapper waits and checks again. A rejected account has a
configurable cooldown (five minutes by default) before it can be retried. Exiting Codex normally exits the
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
An unreadable goal database is treated as unknown state, not as proof that no
goal exists; automatic continuation is withheld in that case. Input held while
waiting for a ready terminal is restored after 15 seconds from the first held
keystroke if readiness cannot be confirmed, allowing manual continuation.

Resuming across accounts sends the selected conversation's context using the
next account. Per-account billing and permissions still apply. The launcher does
not buy credits or alter billing settings. This manages the focused conversation;
it does not migrate all background subagent runtimes or other cmux panes.

## Terminal input and backpressure

The launcher forwards input and output through bounded nonblocking queues.
Partial writes are retried without dropping their unwritten bytes; a full terminal
queue does not block quota polling or the opposite direction. When automatic
continuation is pending, terminal input (including split bracketed pastes) is
held until submission. Recognized terminal query replies pass through so CLI
startup can finish; pasted payloads are never treated as query replies. A draft received before generated text is queued cancels
that automatic submission and restores the draft for manual continuation.
Ctrl-C cancels automatic submission and releases held input. The existing
15-second readiness fallback also restores input when readiness is unknown.

Replacing the installed script does not hot-patch a running Python process.
Existing sessions continue with the code they loaded; the fixes take effect on
their next wrapper launch. No sessions need to be stopped to install the update.

## MCP authentication and account-linked connectors

xswap shares local MCP server definitions through the shared `config.toml`.
Configuration sharing is not a guarantee that every account has the same
authenticated tools. Check the destination account before relying on a connector
after a handoff.

For local HTTP MCP servers, Codex supports `bearer_token_env_var` and
`env_http_headers`. Keep secrets in your existing environment or credential
manager and share the variable names in configuration, rather than making copies
of tokens in each account home. Ensure the environment is available to the
terminal launching the wrapper. This reuses the same service identity; changing
the Codex account does not change that service identity or its permissions.

MCP OAuth logins and account-linked apps are separate from these definitions.
Use the provider's supported authorization flow for any missing connection.
Do not copy an entire credential store, browser cookies, or another account's
`auth.json` to make tools appear connected. The latter contains the Codex login
and must remain isolated for correct account selection. The wrapper does not
export connector credentials, transfer account grants, or automatically broaden
tool access. Account-linked connections may require one initial authorization
per account; normal rotation does not itself require reconnecting each time.

See the [official MCP configuration documentation](https://developers.openai.com/codex/mcp/)
for supported environment-based authentication and MCP login options.

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

# Requires the official Codex CLI; uses only a local mock model server:
python3 tests/auto_swap/native_checkpoint_smoke.py
```

Tests use fake xswap/CLI processes in real PTYs and temporary SQLite fixtures.
They exercise proactive and error-triggered rotation, same-batch and delayed
next-turn races, missing/untrusted callbacks, stale checkpoint leases, draft
pastes, concurrent transcript appends, exact conversation selection, full
permissions, goal accounting preservation, and survival of an unrelated process.
The native smoke test proves that an actual Codex callback blocks the model
request until released and that its turn is already persisted. It sends no
requests to an external model provider. CI runs both layers on macOS and Linux. Recheck compatibility after major Codex CLI updates;
the launcher depends on its session history, terminal-title and log formats.
