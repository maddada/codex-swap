# codex-swap (`xswap`)

A small Rust CLI for running Codex under different ChatGPT accounts, with optional shared conversation history. Inspired by the `cswap run` workflow from [claude-swap](https://github.com/realiti4/claude-swap), using the permanent account-directory approach from [swapdex](https://github.com/youdie006/swapdex).

Each account has one permanent `CODEX_HOME`. Codex owns login and token refresh in that directory. xswap selects the directory and executes the official Codex binary. Different accounts can run concurrently, and launches of the same account use the same credential store.

## Install

Requires Rust 1.85 or newer and the official Codex CLI on PATH. Supports macOS, Linux and WSL. Native Windows is not supported in this version.

```sh
cargo install --git https://github.com/maddada/codex-swap --locked
xswap --version
```

From a checkout:

```sh
cargo install --path . --locked
```

Cargo installs the `xswap` executable into `~/.cargo/bin`. To use `~/.local/bin` instead, add `--root ~/.local` to the install command.

## Set up accounts

Register your existing file-based ChatGPT login without copying or moving its credentials:

```sh
xswap add --alias personal
```

Add another account. Codex opens its normal login flow in a new account directory; your original login stays in place:

```sh
xswap add --login --alias work --share-history
# On a headless computer:
xswap add --login --alias secondary --share-history --device-auth
```

The login flow tells you how to finish signing in. Use the intended account in your browser. If interrupted, the slot stays registered as incomplete:

```sh
xswap login 2
```

You can add more accounts at any time, or adopt an existing file-based account home:

```sh
xswap add --home ~/.codex-profiles/work --alias work --share-history
xswap add --slot 5 --login --alias another --share-history
xswap list
```

Adoption keeps the original directory, credentials and settings. Registering the same directory or the same account twice is refused. Email selection is case-insensitive; if the same email belongs to multiple workspaces, select the slot number or a unique alias.

## Run, resume and fork

```sh
xswap run personal --share-history
xswap run work --share-history
xswap run 2 --share-history -- resume SESSION_ID
xswap run work --share-history -- fork SESSION_ID
xswap run user@example.com --share-history -- --model MODEL "Your prompt"
```

Everything after `--` is passed as individual arguments directly to Codex, without a shell. xswap preserves the working directory, terminal, signals and Codex exit status. It does not add permission-bypass flags. If you want those, put them in your own wrappers:

```sh
x1() { xswap run personal --share-history -- --yolo "$@"; }
xw() { xswap run work --share-history -- --yolo "$@"; }
xr() { x1 resume "$@"; }
xf() { x1 fork "$@"; }
```

Account selection removes inherited `OPENAI_API_KEY`, `CODEX_API_KEY`, `CODEX_ACCESS_TOKEN` and `OPENAI_ACCESS_TOKEN` from the child environment. Registered accounts use Codex's file credential backend. Overrides of that backend, or of `sqlite_home` while sharing history, are rejected rather than silently changing the selected account or conversation store. Other Codex arguments are forwarded unchanged.

## Default account

```sh
xswap switch work
xswap run --share-history     # launches work
xswap --status --json         # cswap-compatible spelling
xswap --switch-to 1           # cswap-compatible spelling
xswap switch default          # back to the original Codex home
```

**The default applies to `xswap run`.** A bare `codex` command continues using its own environment and login. xswap does not overwrite the original `auth.json` to change defaults: doing so could change the account used by a terminal already running against that file. Use `xswap run` in your default shell wrapper if you want it to follow the selection:

```sh
x() { xswap run --share-history -- --yolo "$@"; }
```

The original Codex home is selected until you explicitly switch. `xswap run default` always selects that original home, even when another account is the xswap default. `status` reports the future-launch default, not the account of every running terminal. Explicit `xswap run work` does not change the default. Remembering a last-used personal account separately can stay in your shell wrapper, as with cswap.

## What is shared

New managed homes share existing main-home settings and customizations: `config.toml`, named `*.config.toml` profiles, `AGENTS.md`, `AGENTS.override.md`, `skills`, `hooks`, `hooks.json`, `rules` and `agents`. These are symlinks, so edits are shared. Adopted homes keep their own configuration.

`--share-history` enables sharing with the main Codex home for:

| Item | Purpose |
| --- | --- |
| `sessions/` | Conversation transcripts and resume/fork |
| `archived_sessions/` | Archived transcripts |
| `session_index.jsonl` | Session names and lookup |
| `history.jsonl` | Prompt history |
| `thread-writer-locks/` | One active writer per conversation across accounts |
| SQLite state directory | Thread discovery and related persistent state |

The SQLite directory is selected through Codex's `sqlite_home` configuration, keeping databases and their WAL/SHM files together. If the main `config.toml` specifies `sqlite_home`, xswap uses it; otherwise it uses the main Codex home. An inherited `CODEX_SQLITE_HOME` is not used as the sharing anchor. Account credentials, logs and account-local runtime state remain in their account home. History sharing is remembered once enabled.

Enable sharing when adding an account, before it creates private history. If an adopted or previously private home already contains history at these paths, xswap refuses to overwrite it. Preserve and merge that history explicitly before linking it, or keep that home private. Existing links must point to the chosen main home. xswap also refuses divergent config files or links rather than erasing them. If a Codex operation replaces a shared symlink with a real file, preserve/merge that file before the next shared launch; xswap detects the divergence.

Sharing means conversations are available to all the accounts you opt in. Resuming a conversation sends its context using the newly selected account. Sharing across Claude and Codex is not supported; this tool manages Codex only.

## Reauthenticate and remove

```sh
xswap login work
xswap login work --device-auth
xswap remove work
```

Reauthentication and removal refuse while that account has a live xswap launch lease. Other accounts remain available during login. The lease covers processes launched by xswap, not independently launched `codex` processes; finish those before reauthenticating an adopted home. A changed login identity is reported and requires explicit re-registration rather than silently relabeling the account.

`remove` unregisters the account and prints its retained directory. It does **not** delete credentials or history, and does not log out Codex. Removing the selected default returns future launches to the original home. Slot numbers are not automatically reused.

## JSON interface for integrations

```sh
xswap list --json
xswap status --json
xswap switch work --json
xswap add --alias personal --json
xswap remove work --json
```

Successful JSON operations emit one object to stdout; login messages and diagnostics go to stderr. Account objects contain:

```json
{
  "number": 2,
  "alias": "work",
  "email": "user@example.com",
  "accountId": "account-id",
  "plan": "pro",
  "home": "/path/to/account/home",
  "managed": true,
  "shareHistory": true,
  "isDefault": false,
  "loginStatus": "present"
}
```

Every response has `schemaVersion: 1`. `list` contains `accounts`; `add` contains `account`; `status` and `switch` contain `active`, `defaultHome` and `usesOriginalDefault`; `remove` contains `removed` and `retainedHome`. `active` is null when the original default home has not been registered.

`loginStatus` is `present`, `login_required`, `invalid_credentials`, or `identity_changed`. `present` means structurally valid credentials exist locally; it does **not** promise that the server will accept the token or that quota remains. Identity fields can be null before login completes. No command prints tokens or refreshes them itself. Integrations must tolerate additional fields in future versions.

Usage polling, quota decisions and automatic account selection belong in the consuming application (such as gxserver). xswap performs no usage requests, runs no daemon and provides no proxy. A consumer switches a session by stopping that session at an appropriate point, then launching the chosen account with `resume` and the same session ID.

## Storage and authentication scope

The registry defaults to `$XDG_DATA_HOME/codex-swap` when `XDG_DATA_HOME` is absolute, otherwise `~/.local/share/codex-swap`. It contains `accounts.json`, permanent account directories, and advisory lock files. Registry writes are atomic; registry/account directories use mode 0700 and registry/lock files use mode 0600. Codex writes its own `auth.json` credentials. Protect this runtime directory like your normal Codex home.

| Option / environment | Meaning |
| --- | --- |
| `--data-dir` / `XSWAP_HOME` | Separate xswap registry and managed homes |
| `--codex-home` / `XSWAP_CODEX_HOME` | Main configuration/history home |
| `--codex-bin` / `XSWAP_CODEX_BIN` | Codex executable, default `codex` on PATH |

On first use, the main home defaults to `CODEX_HOME`, then `~/.codex`. It is persisted on the first registry mutation, so an inherited account-specific `CODEX_HOME` does not subsequently move the sharing anchor. A different explicit main home requires a different registry.

This first version manages **file-based ChatGPT logins**. New managed accounts select that backend explicitly through the official login command. It does not import OS-keyring/auto credentials, API-key logins or externally managed tokens. For a keyring-based existing setup, use `xswap add --login` to create an independent login instead of copying a potentially stale `auth.json`. Managed enterprise requirements still apply through Codex itself.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build --release --locked
```

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for reference-code attribution. The project is MIT licensed.
