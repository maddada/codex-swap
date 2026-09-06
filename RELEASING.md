# Releasing codex-swap

The `Release binaries` workflow builds native ARM64 and x86-64 executables for macOS and Linux. macOS targets macOS 11 or newer. Linux uses static musl binaries. Each runner checks `xswap --version` and `--help` before packaging the executable and license documents.

1. Update the version in `Cargo.toml` and `Cargo.lock`, and commit the release changes.
2. Push the commit and matching `vMAJOR.MINOR.PATCH` tag. The tag must match `Cargo.toml`.
3. Wait for `Release binaries` to finish. It publishes four archives, `SHA256SUMS`, and a generated `codex-swap.rb` formula together. A manual workflow run builds the same artifacts without publishing a release.
4. Download `codex-swap.rb` from that release into `Formula/codex-swap.rb` in `maddada/homebrew-tap`. The formula contains checksums of the exact published archives and installs only the prebuilt executable.
5. Run `ruby -c Formula/codex-swap.rb`, `brew style Formula/codex-swap.rb`, and `git diff --check` in the tap. Review and commit only that formula, then push the tap update.
6. Verify `brew install maddada/tap/codex-swap` and `xswap --version` on macOS and Linux. Use `brew upgrade` for an existing installation.

The tap update is separate from the binary workflow, like Ghostex's release tooling. The workflow does not require a token with write access to another repository. Never overwrite archives for a published version: publish a new version and update the formula instead.

To regenerate a formula from locally downloaded release archives:

```sh
python3 scripts/homebrew.py 0.1.0 path/to/archives path/to/Formula/codex-swap.rb
```

This also writes `SHA256SUMS` into the archive directory. All four archives must be present; partial releases cannot produce a formula.
