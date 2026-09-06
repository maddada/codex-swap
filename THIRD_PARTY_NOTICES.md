# Reference code

The Codex identity parser in `src/auth.rs` is adapted from the Codex adapter in [swapdex](https://github.com/youdie006/swapdex), inspected at commit `b253954cb028fac1314b5ca412498b62117ecfeb`. Its permanent account-directory model and sharing list informed the implementation. xswap adds its own scoped command interface, account leases, registry, sharing checks and SQLite-directory selection.

The following notice applies to that adapted code:

```text
MIT License

Copyright (c) 2026 swapdex contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

[claude-swap](https://github.com/realiti4/claude-swap) is a behavioral reference for the selected CLI workflow; no Claude-specific implementation is included.

Codex authentication and state layout were checked against the [official authentication documentation](https://developers.openai.com/codex/auth) and the [OpenAI Codex source](https://github.com/openai/codex). Codex remains a separately installed executable.

## OpenUsage

The Codex quota endpoint, account header, response mapping and burn-rate pacing in `src/usage_client.rs` and `src/usage_model.rs` follow [OpenUsage](https://github.com/robinebers/openusage), inspected at commit `70dea9a8fa21ed205aa9ad625b416a1e7792d5a1`. xswap reads each selected account’s file-based credentials; Codex continues to own token refresh.

The following notice applies to this adapted behavior and code:

```text
MIT License

Copyright (c) 2026 Robin Ebers

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
