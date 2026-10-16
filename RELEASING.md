# Releasing codex-swap

The `Release binaries` workflow builds native ARM64 and x86-64 executables for macOS, Linux, and Windows. Windows uses native MSVC binaries with the C runtime linked statically and ZIP archives on x64 and ARM64 runners. macOS targets macOS 11 or newer. Linux uses static musl binaries. Each runner checks `xswap --version` and `--help` before packaging the executable and license documents.

1. Update the version in `Cargo.toml` and `Cargo.lock`, and commit the release changes.
2. Push the commit and matching `vMAJOR.MINOR.PATCH` tag. The tag must match `Cargo.toml`.
3. Wait for `Release binaries` to finish. It publishes six archives, the Windows `install.ps1` and macOS/Linux `install.sh` installers, `SHA256SUMS`, and a generated `codex-swap.rb` formula together. A manual workflow run builds the same artifacts without publishing a release.
4. The same workflow then publishes the formula to the tap: its last step clones `maddada/homebrew-tap` over SSH with the `HOMEBREW_TAP_DEPLOY_KEY` deploy key, copies the release's `codex-swap.rb` to `Formula/codex-swap.rb`, checks it with `ruby -c`, and, only when the file changed, commits it as `github-actions[bot]` with `chore: update codex-swap formula to X.Y.Z` and pushes `main` (rebasing once if `main` moved). The formula contains checksums of the exact published archives and installs only the prebuilt executable.
5. If that step was skipped (the run shows a `HOMEBREW_TAP_DEPLOY_KEY is not set` notice) or failed, update the tap by hand: download `codex-swap.rb` from the release into `Formula/codex-swap.rb` in `maddada/homebrew-tap`, run `ruby -c Formula/codex-swap.rb`, `brew style Formula/codex-swap.rb`, and `git diff --check` in the tap, review and commit only that formula, then push the tap update.
6. Verify `brew install maddada/tap/codex-swap` and `xswap --version` on macOS and Linux. Use `brew upgrade` for an existing installation.

The tap update is the last step of the publish job and runs only after `gh release create` succeeded, so the tap never points at archives that are not published. The workflow token cannot write to another repository, so the step pushes with a deploy key that has write access to `maddada/homebrew-tap` alone, stored as the `HOMEBREW_TAP_DEPLOY_KEY` secret on `maddada/codex-swap`, and pins GitHub's published SSH host keys. Without the secret the step skips with a notice instead of failing the release. Never overwrite archives for a published version: publish a new version and update the formula instead.

To regenerate a formula from locally downloaded release archives:

```sh
python3 scripts/homebrew.py 0.3.0 path/to/archives path/to/Formula/codex-swap.rb
```

This also writes `SHA256SUMS` into the archive directory. All six archives and both installers (`install.ps1`, `install.sh`) must be present; partial releases cannot produce a formula.

Windows installation and upgrades use the same script, without Cargo:

```powershell
irm https://github.com/maddada/codex-swap/releases/latest/download/install.ps1 | iex
```

The script chooses the host architecture, checks the release archive against `SHA256SUMS`, validates archive members and `xswap --version`, and installs under `%LOCALAPPDATA%\Programs\codex-swap`. It adds that directory to the user PATH. Download the script and use `-Version vMAJOR.MINOR.PATCH`, `-InstallDir PATH`, or `-NoPathUpdate` for explicit control. Upgrade runs may leave a currently running previous executable beside the new version; the next installer run removes it after it exits. Failed replacement restores the previous executable.

macOS and Linux use the POSIX `install.sh` the same way, without Homebrew or Cargo:

```sh
curl -fsSL https://github.com/maddada/codex-swap/releases/latest/download/install.sh | sh
```

It learns the latest version from the release's `SHA256SUMS` (so it never calls the rate-limited GitHub API), downloads that tag's archive, verifies it, validates the four archive members and `xswap --version`, and installs to `~/.local/bin` (`XSWAP_INSTALL_DIR` overrides; `XSWAP_VERSION` pins a release). It writes `.xswap-install-receipt.json` beside the executable with the method, directory, version, target and executable SHA-256, which `xswap upgrade` uses to rerun the embedded copy of the script.

Account data on Windows lives in `%LOCALAPPDATA%\codex-swap` by default and uses owner-only Windows ACLs. Shared configuration and history use true symbolic links, so enable Windows Developer Mode or grant the Create symbolic links privilege before adding managed accounts. Hardlinks are intentionally unsuitable because Codex can replace files atomically. CI validates the installer syntax and checks the executable version and help on both native Windows architectures. macOS cross-checks cannot replace those native runtime checks.
