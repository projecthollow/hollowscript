"""Canonical tar metadata and exact builder-path checks for release scripts."""
import gzip
import pathlib
import re
import sys
import tarfile


def contains_path(data, needle):
    pattern = rb"(?<![A-Za-z0-9_./\\-])" + re.escape(needle.encode()) + rb"(?=[/\\\x00\s\"':]|$)"
    return re.search(pattern, data) is not None


def archive(source, destination, epoch):
    source, destination = pathlib.Path(source), pathlib.Path(destination)
    paths = [source] + sorted(source.rglob("*"))
    if any(path.is_symlink() or not (path.is_file() or path.is_dir()) for path in paths):
        raise ValueError("release archive must contain regular files and directories only")
    with destination.open("wb") as output:
        with gzip.GzipFile(filename="", mode="wb", fileobj=output, mtime=0, compresslevel=9) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as tar:
                for path in paths:
                    info = tar.gettarinfo(str(path), arcname=str(path.relative_to(source.parent)))
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    info.mtime = epoch
                    info.mode = 0o755 if path.is_dir() or path.stat().st_mode & 0o111 else 0o644
                    if path.is_file():
                        with path.open("rb") as content:
                            tar.addfile(info, content)
                    else:
                        tar.addfile(info)


if __name__ == "__main__":
    if sys.argv[1] == "archive":
        archive(sys.argv[2], sys.argv[3], int(sys.argv[4]))
    elif sys.argv[1] == "check-path":
        data = pathlib.Path(sys.argv[2]).read_bytes()
        if any(contains_path(data, needle) for needle in sys.argv[3:]):
            print("release path check: a builder path is present", file=sys.stderr)
            sys.exit(1)
    else:
        raise SystemExit("unknown release helper operation")
