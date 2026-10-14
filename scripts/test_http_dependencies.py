"""Keep dependency upgrades from silently changing the HTTP trust backend."""

from pathlib import Path
import tomllib
import unittest


class HttpDependencyTests(unittest.TestCase):
    def test_reqwest_features_preserve_the_reviewed_native_tls_backend(self):
        root = Path(__file__).resolve().parent.parent
        with (root / "Cargo.toml").open("rb") as source:
            dependency = tomllib.load(source)["dependencies"]["reqwest"]
        self.assertIs(dependency.get("default-features"), False)
        # Reqwest 0.13 changed its default backend; retain the former native
        # TLS without ALPN selection and the application's other features.
        self.assertEqual(
            set(dependency["features"]),
            {"native-tls-no-alpn", "charset", "http2", "system-proxy", "json", "stream"},
        )


if __name__ == "__main__":
    unittest.main()
