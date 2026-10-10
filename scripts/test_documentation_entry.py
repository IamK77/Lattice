"""Check the entry docs' inline fragments and bilingual executable examples.

This covers the repository's ATX headings, explicit HTML ids and triple-backtick
fences, not arbitrary Markdown extensions or the reachability of external URLs.
"""
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parent.parent
PAIRS = [
    ("README.md", "README.zh-CN.md"),
    ("docs/README.md", "docs/README.zh-CN.md"),
    ("docs/getting-started.md", "docs/getting-started.zh-CN.md"),
    ("docs/model-configuration.md", "docs/model-configuration.zh-CN.md"),
    ("docs/desktop.md", "docs/desktop.zh-CN.md"),
    ("clients/ink/README.md", "clients/ink/README.zh-CN.md"),
]
DOCUMENTS = [name for pair in PAIRS for name in pair] + [
    "docs/setup-development.md", "docs/desktop-development.zh-CN.md",
    "clients/ink/development.zh-CN.md", "docs/installation.md",
    "docs/installation.zh-CN.md", "docs/troubleshooting.md",
    "docs/troubleshooting.zh-CN.md", "SUPPORT.md", "CONTRIBUTING.md",
    "docs/development.md", "docs/development.zh-CN.md",
    "docs/contracts/README.md", "docs/contracts/README.zh-CN.md",
    "docs/contracts/01-envelope.zh-CN.md", "docs/contracts/02-core-events.zh-CN.md",
    "docs/contracts/03-component-manifest.zh-CN.md", "docs/contracts/04-assembly-manifest.zh-CN.md",
    "docs/contracts/05-standard-interfaces.zh-CN.md", "docs/contracts/06-process-bridge.zh-CN.md",
    "docs/design-expert-snapshot-prototype.zh-CN.md", "docs/design-expert-execution.zh-CN.md",
    "docs/expert-management-prototype.zh-CN.md", "docs/release-publication.md",
    "docs/release-publication.zh-CN.md",
]
FENCES = re.compile(r"(?ms)^```([\w-]*)\n(.*?)^```[ \t]*$")


def anchors(text):
    prose = FENCES.sub("", text)
    found = set(re.findall(r'<[^>]+\bid=[\"\']([^\"\']+)[\"\']', prose))
    generated = set()
    for heading in re.findall(r"(?m)^#{1,6} +(.+?) *#* *$", prose):
        base = re.sub(r"[^\w\- ]", "", heading.lower()).replace(" ", "-")
        candidate, suffix = base, 0
        while candidate in generated:
            suffix += 1
            candidate = f"{base}-{suffix}"
        generated.add(candidate)
    return found | generated


def broken_fragments(root, name):
    path = root / name
    prose = FENCES.sub("", path.read_text(encoding="utf-8"))
    errors = []
    for link in re.findall(r"\]\(([^)\n]+)\)", prose):
        parsed = urlsplit(link)
        if parsed.scheme or parsed.netloc or not parsed.fragment:
            continue
        target = (path.parent / unquote(parsed.path)).resolve() if parsed.path else path.resolve()
        if not target.is_relative_to(root.resolve()):
            errors.append(f"{name}: target leaves repository: {link}")
        elif target.suffix == ".md":
            if not target.is_file():
                errors.append(f"{name}: missing file: {link}")
            elif unquote(parsed.fragment) not in anchors(target.read_text(encoding="utf-8")):
                errors.append(f"{name}: missing anchor: {link}")
    return errors


def shell_blocks(text):
    return [(language, body) for language, body in FENCES.findall(text)
            if language in {"bash", "sh"}]


class DocumentationEntryTests(unittest.TestCase):
    def test_entry_document_fragments_resolve(self):
        for name in DOCUMENTS:
            with self.subTest(document=name):
                self.assertEqual(broken_fragments(ROOT, name), [])

    def test_bilingual_shell_examples_match(self):
        for english, chinese in PAIRS:
            with self.subTest(document=english):
                left = shell_blocks((ROOT / english).read_text(encoding="utf-8"))
                right = shell_blocks((ROOT / chinese).read_text(encoding="utf-8"))
                self.assertTrue(left, "each pair must exercise executable examples")
                self.assertEqual(left, right)

    def test_entry_shell_examples_parse_without_execution(self):
        checked = 0
        for name in DOCUMENTS:
            for index, (_, body) in enumerate(shell_blocks((ROOT / name).read_text(encoding="utf-8"))):
                with self.subTest(document=name, block=index):
                    result = subprocess.run(["bash", "-n"], input=body, text=True,
                                            capture_output=True, check=False)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    checked += 1
        self.assertGreater(checked, 0)

    def test_user_bookmarks_survive_extraction(self):
        for name, expected in [
            ("docs/getting-started.md", {"3-start-in-your-project", "manual-configuration-optional", "data-and-permissions"}),
            ("docs/getting-started.zh-CN.md", {"3-在自己的项目里启动", "手动配置可选", "数据与权限"}),
            ("clients/ink/README.md", {"run", "历史分页", "操作授权", "the-protocol-in-one-screen", "layout", "tests"}),
            ("docs/desktop.zh-CN.md", {"安装与系统许可", "使用方式", "保护与边界", "验证"}),
        ]:
            with self.subTest(document=name):
                self.assertTrue(expected <= anchors((ROOT / name).read_text(encoding="utf-8")))

    def test_heading_and_explicit_anchors_ignore_fenced_examples(self):
        text = '# Title\n## 使用 Lattice\n## Repeat\n## Repeat\n<a id="previous"/>\n```text\n## Fake\n```\n'
        self.assertEqual(anchors(text), {"title", "使用-lattice", "repeat", "repeat-1", "previous"})

    def test_fragment_check_detects_missing_and_escaped_targets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "guide.md").write_text("# Guide\n## 中文\n", encoding="utf-8")
            (root / "index.md").write_text(
                "[good](guide.md#%E4%B8%AD%E6%96%87)\n"
                "[bad](guide.md#absent)\n[missing](missing.md#part)\n"
                "[outside](../private.md#secret)\n"
                "[external](https://example.invalid/guide.md#absent)\n"
                "```text\n[fenced](missing.md#ignored)\n```\n", encoding="utf-8",
            )
            errors = broken_fragments(root, "index.md")
            self.assertEqual(errors, [
                "index.md: missing anchor: guide.md#absent",
                "index.md: missing file: missing.md#part",
                "index.md: target leaves repository: ../private.md#secret",
            ])


if __name__ == "__main__":
    unittest.main()
