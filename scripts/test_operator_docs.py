"""Keep executable documentation syntax and bilingual installation steps aligned."""
from pathlib import Path
import re
import subprocess
import unittest


ROOT = Path(__file__).resolve().parent.parent


def blocks(name):
    return re.findall(r"(?ms)^```bash\n(.*?)^```[ \t]*$", (ROOT / "docs" / name).read_text())


def without_comments(block):
    # These installation snippets have only whole-line comments or a comment
    # introduced after two spaces, never a quoted hash with that prefix.
    return [line.split("  #", 1)[0].rstrip() for line in block.splitlines()
            if line.strip() and not line.lstrip().startswith("#")]


class OperatorDocumentationTests(unittest.TestCase):
    def test_all_operator_bash_examples_parse_without_execution(self):
        for name in ["installation.md", "installation.zh-CN.md", "troubleshooting.md", "troubleshooting.zh-CN.md",
                     "signed-preview-acceptance.md", "signed-preview-acceptance.zh-CN.md"]:
            snippets = blocks(name)
            self.assertTrue(snippets, name)
            for index, snippet in enumerate(snippets):
                with self.subTest(document=name, block=index):
                    result = subprocess.run(["bash", "-n"], input=snippet, text=True, capture_output=True, check=False)
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_bilingual_installation_and_verification_commands_do_not_drift(self):
        for english_name, chinese_name in [("installation.md", "installation.zh-CN.md"),
                                           ("signed-preview-acceptance.md", "signed-preview-acceptance.zh-CN.md")]:
            with self.subTest(document=english_name):
                english = [without_comments(block) for block in blocks(english_name)]
                chinese = [without_comments(block) for block in blocks(chinese_name)]
                self.assertEqual(english, chinese)


if __name__ == "__main__":
    unittest.main()
