import io
import unittest
from unittest.mock import MagicMock, patch

from github_release import GitHub


class RepositoryPathTests(unittest.TestCase):
    def test_full_commit_comparison_uses_fixed_repository_host(self):
        path = "/compare/" + "a" * 40 + "..." + "b" * 40
        opener = MagicMock()
        opener.open.return_value = io.BytesIO(b'{"ahead_by": 1}')
        with patch("github_release.build_opener", return_value=opener):
            result = GitHub("IamK77/Lattice", "fixture-token").request("GET", path)
        self.assertEqual(result, {"ahead_by": 1})
        request = opener.open.call_args.args[0]
        self.assertEqual(request.full_url, "https://api.github.com/repos/IamK77/Lattice" + path)
        self.assertEqual(request.get_method(), "GET")

    def test_comparison_exception_does_not_admit_arbitrary_dot_paths(self):
        paths = ["/../elsewhere", "/compare/main...develop", "/compare/abc...def",
                 "/compare/" + "a" * 40 + "..." + "b" * 40 + "/../releases",
                 "https://elsewhere.test/path", "releases"]
        with patch("github_release.build_opener") as opener:
            for path in paths:
                with self.subTest(path=path), self.assertRaises(ValueError):
                    GitHub("IamK77/Lattice", "fixture-token").request("GET", path)
            opener.assert_not_called()


if __name__ == "__main__":
    unittest.main()
