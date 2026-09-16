"""One invocation; files are explicitly published only after successful execution."""
import json
import os
from pathlib import Path


class Artifacts:
    def __init__(self):
        self._root = Path.cwd().resolve()
        self._entries = []

    def publish(self, path, *, name=None):
        path = Path(path)
        if path.is_absolute() or ".." in path.parts:
            raise ValueError("publish requires a relative path inside this execution")
        relative = path.resolve().relative_to(self._root)
        self._entries.append({"path": str(relative), "name": name or path.name})
        if len(self._entries) > 16:
            raise ValueError("At most 16 artifacts may be published")

    def finish(self):
        (self._root / ".artifacts.json").write_text(json.dumps(self._entries), encoding="utf-8")


os.environ["MPLBACKEND"] = "Agg"
artifacts = Artifacts()
source = Path("calculation.py")
exec(compile(source.read_text(encoding="utf-8"), str(source), "exec"),
     {"__name__": "__main__", "__file__": str(source.resolve()), "artifacts": artifacts})
artifacts.finish()
