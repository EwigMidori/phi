# phi-code-cli

Thin binary entry for phi-code: process env + terminal host over `phi-code-core` + `phi-code-ui`.

## LLM config

Product runtime: **`phi-code-core`** (`TurnDriver` + `SessionHost`). Wire adapter:
**`phi-ext-llm`** (composed inside core). This binary only loads `PHI_*` via
`ProcessEnv` and runs the TUI loop.

Reads **only** the process cwd’s `.env` via `dotenvy` (no parent-directory walk), plus already-exported env vars.

```powershell
# From a directory that contains .env with PHI_*:
cd path\to\dir-with-dotenv
cargo run -p phi-code-cli
```

Or export env explicitly:

```powershell
$env:PHI_API_KEY = "sk-..."
$env:PHI_MODEL = "deepseek-v4-flash"
$env:PHI_API_BASE = "https://api.deepseek.com"
$env:PHI_API_STYLE = "responses"
# Optional: context window for the status bar (used / available), default 128000
# $env:PHI_CONTEXT_WINDOW = "1000000"

cargo run -p phi-code-cli
```

Status bar token segment: **`used / window`** from provider `prompt_tokens` (or `total_tokens`) over `PHI_CONTEXT_WINDOW`, humanized (`12K / 128K`, `1M`, …). Not estimated.

Do **not** commit API keys.
