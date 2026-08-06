# phi-code-cli

Thin binary entry for phi-code.

## LLM config

Wire adapter: **`phi-ext-llm`** (`OpenAiCompatRuntime`). Composition is in this
binary; `phi-code-core` only runs turns.

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

cargo run -p phi-code-cli
```

Do **not** commit API keys.
