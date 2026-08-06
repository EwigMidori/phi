# phi-code-cli

Thin binary entry for phi-code.

## LLM config

Reads **only** the process cwd’s `.env` via `dotenvy` (no parent-directory walk), plus already-exported env vars.

```powershell
# From a directory that contains .env with AGENT_LLM__*:
cd path\to\dir-with-dotenv
cargo run -p phi-code-cli --manifest-path E:\...\open-source\Cargo.toml
```

Or export env explicitly:

```powershell
$env:AGENT_LLM__API_KEY = "sk-..."
$env:AGENT_LLM__MODEL = "deepseek-v4-flash"
$env:AGENT_LLM__BASE_URL = "https://api.deepseek.com"
$env:AGENT_LLM__API_STYLE = "responses"

cargo run -p phi-code-cli
```

Do **not** commit API keys.
