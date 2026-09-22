"""One invocation; files are explicitly published only after successful execution."""
import json
import os
import sys
from pathlib import Path
from types import ModuleType


class Artifacts(ModuleType):
    def __init__(self):
        super().__init__("artifacts", "Publish files from the current run_python invocation.")
        self._root = Path.cwd().resolve()
        self._entries = []

    def publish(self, path, *, name=None):
        """Register an existing file; save it first in this same execution."""
        path = Path(path)
        if path.is_absolute() or ".." in path.parts:
            raise ValueError("publish requires a relative path inside this execution")
        relative = path.resolve().relative_to(self._root)
        if not path.exists():
            raise FileNotFoundError(
                f"Cannot publish {str(path)!r}: file does not exist. "
                "publish() registers an existing file; it does not create or save one. "
                "Save it first in this same run_python call using fig.savefig(...) for plots "
                f"or df.to_csv(...) for CSV, then artifacts.publish({str(path)!r}). "
                "Each call uses a new temporary directory."
            )
        if not path.is_file():
            raise ValueError(f"Cannot publish {str(path)!r}: expected a file, not a directory")
        self._entries.append({"path": str(relative), "name": name or path.name})
        if len(self._entries) > 16:
            raise ValueError("At most 16 artifacts may be published")

    def finish(self):
        (self._root / ".artifacts.json").write_text(json.dumps(self._entries), encoding="utf-8")


os.environ["MPLBACKEND"] = "Agg"
artifacts = Artifacts()
sys.modules["artifacts"] = artifacts
source = Path("calculation.py")
exec(compile(source.read_text(encoding="utf-8"), str(source), "exec"),
     {"__name__": "__main__", "__file__": str(source.resolve()), "artifacts": artifacts})
artifacts.finish()
