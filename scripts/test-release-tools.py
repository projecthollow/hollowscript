"""Release packaging and path-boundary regressions."""
import importlib.util
import pathlib
import tempfile
import unittest
import tarfile

spec = importlib.util.spec_from_file_location("release_tools", pathlib.Path(__file__).with_name("release-tools.py"))
tools = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tools)


class ReleaseToolsTests(unittest.TestCase):
    def test_real_path_is_rejected_without_matching_library_source_suffix(self):
        self.assertFalse(tools.contains_path(b"library/core/src/lib.rs", "/src"))
        self.assertFalse(tools.contains_path(b"/src-other/a", "/src"))
        self.assertTrue(tools.contains_path(b"\0/src/project/a.rs\0", "/src"))
        self.assertTrue(tools.contains_path(b"path=/build/checkout/a.rs", "/build/checkout"))

    def test_archive_is_independent_of_owner_mtime_and_creation_order(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            for tree, names in ((root / "a" / "payload", ["b", "a"]), (root / "b" / "payload", ["a", "b"])):
                tree.mkdir(parents=True)
                for name in names:
                    (tree / name).write_text(name)
            tools.archive(root / "a" / "payload", root / "one.tar.gz", 42)
            tools.archive(root / "b" / "payload", root / "two.tar.gz", 42)
            self.assertEqual((root / "one.tar.gz").read_bytes(), (root / "two.tar.gz").read_bytes())
            with tarfile.open(root / "one.tar.gz") as tar:
                self.assertTrue(all(m.uid == m.gid == 0 and m.mtime == 42 and m.uname == m.gname == "" for m in tar))

    def test_archive_refuses_symlinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            tree = root / "payload"
            tree.mkdir()
            (tree / "link").symlink_to("/outside")
            with self.assertRaises(ValueError):
                tools.archive(tree, root / "bad.tar.gz", 42)


if __name__ == "__main__":
    unittest.main()
