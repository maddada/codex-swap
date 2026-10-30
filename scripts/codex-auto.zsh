# Source after PATH setup in ~/.zshrc. Only interactive cmux invocations rotate.
# The real Codex executable remains intact; `command codex` bypasses this function.
codex() {
  if [[ -t 0 && -t 1 && "${CODEX_AUTO_BYPASS:-0}" != 1 &&
        -n "${CMUX_SURFACE_ID:-}${CMUX_WORKSPACE_ID:-}${CMUX_SOCKET_PATH:-}" ]]; then
    local auto_bin="${CODEX_AUTO_BIN_DIR:-$HOME/.local/bin}"
    local cmux_wrapper="${CODEX_AUTO_CMUX_WRAPPER:-}"
    if [[ -z "$cmux_wrapper" && -n "${CMUX_BUNDLED_CLI_PATH:-}" ]]; then
      cmux_wrapper="${CMUX_BUNDLED_CLI_PATH:h}/cmux-codex-wrapper"
    fi
    if [[ ! -x "$cmux_wrapper" ]]; then
      cmux_wrapper="/Applications/cmux.app/Contents/Resources/bin/cmux-codex-wrapper"
    fi
    if [[ -x "$cmux_wrapper" ]]; then
      CMUX_CUSTOM_CODEX_PATH="$auto_bin/codex-auto-cli" command "$cmux_wrapper" "$@"
    else
      command "$auto_bin/codex-auto" --cli -- "$@"
    fi
  else
    command codex "$@"
  fi
}
