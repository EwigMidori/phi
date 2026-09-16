"""Exercise the real bootstrap in isolated Python processes, as the executor does."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest


BOOTSTRAP = Path(__file__).resolve().parents[1] / "src" / "python_bootstrap.py"


class PythonBootstrapTests(unittest.TestCase):
    def run_code(self, code):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        (root / "bootstrap.py").write_bytes(BOOTSTRAP.read_bytes())
        (root / "calculation.py").write_text(textwrap.dedent(code), encoding="utf-8")
        result = subprocess.run(
            [sys.executable, "-I", "-X", "utf8", "-u", str(root / "bootstrap.py")],
            cwd=root, capture_output=True, text=True, encoding="utf-8", timeout=10,
        )
        return root, result

    def test_direct_and_imported_publishers_collect_together(self):
        root, result = self.run_code('''
            from pathlib import Path
            Path("direct.txt").write_text("direct", encoding="utf-8")
            artifacts.publish("direct.txt")
            injected = artifacts
            import artifacts
            assert artifacts is injected
            Path("module.txt").write_text("module", encoding="utf-8")
            artifacts.publish("module.txt", name="renamed.txt")
            from artifacts import publish
            Path("imported.txt").write_text("imported", encoding="utf-8")
            publish("imported.txt")
        ''')
        self.assertEqual(result.returncode, 0, result.stderr)
        entries = json.loads((root / ".artifacts.json").read_text(encoding="utf-8"))
        published = [
            (entry["name"], (root / entry["path"]).read_text(encoding="utf-8"))
            for entry in entries
        ]
        self.assertEqual(published, [
            ("direct.txt", "direct"), ("renamed.txt", "module"),
            ("imported.txt", "imported"),
        ])
        next_root, next_result = self.run_code("import artifacts")
        self.assertEqual(next_result.returncode, 0, next_result.stderr)
        self.assertEqual(
            json.loads((next_root / ".artifacts.json").read_text(encoding="utf-8")), [],
        )

    def test_imported_publisher_does_not_commit_after_failure(self):
        root, result = self.run_code('''
            from pathlib import Path
            from artifacts import publish
            Path("partial.txt").write_text("partial", encoding="utf-8")
            publish("partial.txt")
            raise RuntimeError("calculation failed")
        ''')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("calculation failed", result.stderr)
        self.assertFalse((root / ".artifacts.json").exists())

    def test_imports_share_the_publication_limit(self):
        root, result = self.run_code('''
            from pathlib import Path
            Path("data.txt").write_text("data", encoding="utf-8")
            for _ in range(16):
                artifacts.publish("data.txt")
            from artifacts import publish
            publish("data.txt")
        ''')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("At most 16 artifacts", result.stderr)
        self.assertFalse((root / ".artifacts.json").exists())


if __name__ == "__main__":
    unittest.main()
