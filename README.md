# codex-swap (`xswap`)

A CLI for running Codex under different ChatGPT accounts, with usage reporting, directory mappings, portable account backups and optional shared conversation history. Inspired by the `cswap run` workflow from [claude-swap](https://github.com/realiti4/claude-swap), using the permanent account-directory approach from [swapdex](https://github.com/youdie006/swapdex).

Each account has one permanent `CODEX_HOME`. Codex owns login and token refresh in that directory. xswap selects the directory and executes the official Codex binary. Different accounts can run concurrently, and launches of the same account use the same credential store.

## Install

Supports macOS 11 or newer, Linux, WSL and native Windows on ARM64 and x86-64. Install the official Codex CLI separately and make sure `codex` is on PATH.

### Homebrew (recommended)

Add the tap and [trust the Codex Swap formula](https://docs.brew.sh/Tap-Trust), then install:

```sh
brew tap maddada/tap && brew trust --formula maddada/tap/codex-swap && brew install maddada/tap/codex-swap && xswap --version
```

Homebrew installs a prebuilt `xswap` executable. Rust and Cargo are not required. Linux releases are statically linked with musl, so they do not depend on a particular glibc version.

To update:

```sh
xswap upgrade
```

On macOS and Linux this runs `brew upgrade maddada/tap/codex-swap`. Homebrew must own the installation for this command to upgrade it.

### Windows (PowerShell)

Install or upgrade the native executable with one command:

```powershell
irm https://github.com/maddada/codex-swap/releases/latest/download/install.ps1 | iex
```

The installer selects x64 or ARM64, verifies the archive against the release’s SHA-256 checksums and installs to `%LOCALAPPDATA%\Programs\codex-swap`, adding it to your user PATH. No Cargo or administrator access is needed for installation. Open a new terminal afterward. `xswap upgrade` runs the same installer for the directory containing your current executable.

Managed accounts share configuration through symbolic links. Enable **Windows Developer Mode** (or grant your account the Create symbolic links privilege) before creating or importing managed accounts or enabling shared history. Existing adopted homes can run without shared links. npm’s `codex.cmd` launcher is supported. Windows batch launchers use Rust’s batch-file handling through `cmd.exe`, which can reject special-character or multiline arguments. Use a native `codex.exe` via `--codex-bin` for those prompts.

For an explicit version or installation directory, download the script and run it locally:

```powershell
Invoke-WebRequest https://github.com/maddada/codex-swap/releases/latest/download/install.ps1 -OutFile install.ps1
.\install.ps1 -Version v0.2.0 -InstallDir "$env:LOCALAPPDATA\Programs\codex-swap"
```

Use `-NoPathUpdate` to manage PATH yourself. The installer also upgrades existing installations; a running old executable is retired and cleaned up by a later installer run after it exits.

### Download a binary

Download the archive for your computer and `SHA256SUMS` from [GitHub Releases](https://github.com/maddada/codex-swap/releases/latest):

| Computer | Archive target |
| --- | --- |
| macOS Apple Silicon | `aarch64-apple-darwin` |
| macOS Intel | `x86_64-apple-darwin` |
| Linux / WSL ARM64 | `aarch64-unknown-linux-musl` |
| Linux / WSL x86-64 | `x86_64-unknown-linux-musl` |
| Windows ARM64 | `aarch64-pc-windows-msvc` |
| Windows x64 | `x86_64-pc-windows-msvc` |

Verify the downloaded archive against its entry in `SHA256SUMS` using `shasum -a 256` on macOS, `sha256sum` on Linux or `Get-FileHash -Algorithm SHA256` in PowerShell. Unix archives contain `xswap`; Windows ZIP archives contain `xswap.exe`. Extract the executable into a directory on PATH. No compiler is needed.

### Build from source

Developers building from source need Rust 1.85 or newer:

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

Everything after `--` is passed as individual arguments to Codex. Unix and native Windows `.exe` launches do not use a shell; Windows `.cmd`/`.bat` launchers have the batch-file limitations described above. xswap preserves the working directory, terminal and Codex exit status. On Windows, Codex and its descendants are tied to the launcher’s lifetime: closing or killing xswap ends that spawned process tree. It does not add permission-bypass flags. If you want those, put them in your own wrappers:

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

## Directory mappings

```sh
xswap map work ~/projects/company
xswap map personal ~/projects/company/personal-tool
xswap map                          # list mappings
xswap run                          # choose the mapping for this directory
xswap unmap ~/projects/company/personal-tool
```

A mapping applies to its directory and all subfolders. The nearest mapped ancestor wins. Paths are canonicalized, so a symlink to the same project uses the same mapping. Omit the directory in `map ACCOUNT` or `unmap` to use the current directory. `unmap` removes only the exact directory’s mapping; removing a nested mapping reveals its parent’s mapping again.

Selection order is an explicit `xswap run ACCOUNT`, then the nearest directory mapping, then the saved global default. `xswap run default` explicitly chooses the original Codex home. `status` and `switch` continue to report and change the global default. Mapping `default` requires the original home to be registered first with `xswap add`.

Mappings stay attached to their account when you rename aliases or move/swap slots. Removing an account clears its mappings. If a mapped account is disabled, a bare launch fails with a clear message instead of choosing another account.

## Usage and pacing

```sh
xswap usage                        # saved global default
xswap usage work
xswap usage --all
xswap usage --all --json
```

Usage reports each available Codex quota window’s percentage used and remaining, duration, reset timestamp and time until reset. It also includes additional model-specific or code-review windows and extra-use credits when the service reports them. A weekly window is identified by its reported duration even when Codex sends it in the primary slot.

Pacing estimates end-of-window usage from the amount used and time elapsed. `ahead` means at least 10% projected quota remains; `on_track` means less than 10% remains; `behind` means the quota is projected to run out before reset. An exhaustion estimate appears only when it precedes reset. Pacing is unavailable for zero usage, a missing/expired reset window, or until at least 60 seconds and 1% of the window have elapsed. It is an estimate of a changing usage rate.

The API request and pacing follow [OpenUsage](https://github.com/robinebers/openusage). xswap reads the account’s current Codex access token and account ID to request `https://chatgpt.com/backend-api/wham/usage`. Codex still owns token refresh. If a token is expired, run Codex for that account to refresh it or use `xswap login ACCOUNT`. `list` and `status` remain local and make no usage requests. `--all` includes disabled accounts and reports account failures individually; it exits unsuccessfully if any report fails while preserving successful reports in the output.

## Manage accounts

```sh
xswap rename work company
xswap rename company --clear
xswap move 2 5                     # swaps if slot 5 is occupied
xswap swap 1 5
xswap disable 5
xswap enable 5
```

Aliases must be unique, ignoring case. Moving or swapping changes only slot numbers, preserving account homes, credentials, directory mappings and the account selected as global default. `disable` prevents implicit selection and choosing that account as a new default or mapping. It does not erase credentials, stop running sessions or remove a saved default/mapping. Explicit commands such as `xswap run 5` still work; a disabled implicit choice produces an error until you enable it or explicitly select another account.

## Back up and migrate accounts

```sh
xswap export accounts-backup.json
xswap export work-backup.json --account work
xswap import accounts-backup.json
xswap import accounts-backup.json --remap-slots
```

**Backups contain plaintext login credentials.** xswap creates a new private JSON file and refuses to overwrite an existing file. Keep backups private and transfer them securely. The backup includes account credentials, aliases, slot numbers, enabled state, history-sharing preferences and the selected default. It excludes conversation history, configuration files, directory mappings and machine-local paths.

Import creates fresh managed account homes and validates all accounts before saving the registry. Imported shared settings/history use the destination computer’s main Codex home. Duplicate identities or aliases are refused. Occupied slots are refused unless `--remap-slots` is provided; remapping allocates free slots and prints the assignments. Failed validation leaves the saved registry unchanged and removes staged credentials. An import restores its backed-up default into an empty registry; an established registry keeps its existing global default.

Export refuses while a selected account is running under an xswap lease, so finish that launch first. Imported credentials do not invalidate the original copy, but Codex’s refresh-token behavior still applies when using copies on multiple machines.

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

## Configuration and cleanup

```sh
xswap config
xswap config path
xswap config get codex-bin
xswap config set codex-bin /path/to/codex
xswap config set default-account work
xswap config unset codex-bin
xswap config unset default-account
```

The supported preferences are `codex-bin` (default `codex`) and `default-account` (default `default`, the original Codex home). Preferences live in the registry shown by `config path`. `default-account` accepts the same slot, alias, email or `default` identifiers as `switch`; it stores a slot so renames and moves keep selecting the same account. `config get` and `config list` show the saved preference or its default. For launches/login, executable selection is `--codex-bin`, then `XSWAP_CODEX_BIN`, then the saved preference, then `codex` on PATH. A relative executable path saved by `config set` is resolved when you set it.

To erase xswap’s registry, preferences, mappings and managed account credentials/history:

```sh
xswap purge
# Noninteractive, explicitly confirm the deletion:
xswap purge --yes
```

`purge` requires typing `purge` at its interactive prompt or passing `--yes`. It deletes the managed `accounts/` tree, including homes retained by earlier `remove` commands. Original and adopted Codex homes, and the targets of shared settings/history links, remain intact. It refuses if a managed tree overlaps an original/adopted home, or an account has a live xswap lease. Advisory lock files remain so concurrent processes keep using the same locks. The installed executable is retained; use Homebrew or your installer directory to uninstall it separately.

## JSON interface for integrations

```sh
xswap list --json
xswap status --json
xswap switch work --json
xswap add --alias personal --json
xswap remove work --json
xswap map --json
xswap usage --all --json
xswap config get codex-bin --json
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
  "enabled": true,
  "shareHistory": true,
  "isDefault": false,
  "loginStatus": "present"
}
```

Every response has `schemaVersion: 1`. `list` contains `accounts`; `add` contains `account`; `status` and `switch` contain `active`, `defaultHome` and `usesOriginalDefault`; `remove` contains `removed` and `retainedHome`. `active` is null when the original default home has not been registered.

`loginStatus` is `present`, `login_required`, `invalid_credentials`, or `identity_changed`. `present` means structurally valid credentials exist locally; it does **not** promise that the server will accept the token or that quota remains. Identity fields can be null before login completes. Console and JSON status output omit tokens; `export` writes credentials only to the requested backup file. No command rotates tokens itself. Integrations must tolerate additional fields in future versions.

`usage` performs an on-demand request. xswap runs no daemon or proxy and does not automatically switch accounts near quota limits. Continuous polling and automatic switching policy can remain in a consuming application such as gxserver. A consumer switches a session by stopping that session at an appropriate point, then launching the chosen account with `resume` and the same session ID.

## Storage and authentication scope

On macOS/Linux, the registry defaults to `$XDG_DATA_HOME/codex-swap` when `XDG_DATA_HOME` is absolute, otherwise `~/.local/share/codex-swap`. On native Windows it defaults to `%LOCALAPPDATA%\codex-swap`. It contains `accounts.json`, permanent account directories, and advisory lock files. Registry writes are atomic. On Unix, registry/account directories use mode 0700 and registry/lock files use mode 0600; Windows uses owner-only ACLs. Codex writes its own `auth.json` credentials. Protect this runtime directory like your normal Codex home.

| Option / environment | Meaning |
| --- | --- |
| `--data-dir` / `XSWAP_HOME` | Separate xswap registry and managed homes |
| `--codex-home` / `XSWAP_CODEX_HOME` | Main configuration/history home |
| `--codex-bin` / `XSWAP_CODEX_BIN` | Codex executable, default `codex` on PATH |

On first use, the main home defaults to `CODEX_HOME`, then `~/.codex` (`%USERPROFILE%\.codex` on Windows). It is persisted on the first registry mutation, so an inherited account-specific `CODEX_HOME` does not subsequently move the sharing anchor. A different explicit main home requires a different registry.

xswap manages **file-based ChatGPT logins**. New managed accounts select that backend explicitly through the official login command. It does not import OS-keyring/auto credentials, API-key logins or externally managed tokens. For a keyring-based existing setup, use `xswap add --login` to create an independent login instead of copying a potentially stale `auth.json`. Managed enterprise requirements still apply through Codex itself.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build --release --locked
```

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for reference-code attribution. The project is MIT licensed.
