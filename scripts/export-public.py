"""Copy the tracked public source files into an empty directory."""

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
PRIVATE_DIRECTORIES = {"docs", "promo", "site"}
PRIVATE_FILES = {".github/workflows/release.yml"}
GENERATED_DIRECTORIES = {".git", "__pycache__", "dist", "node_modules", "out", "target"}
GENERATED_SUFFIXES = {".exe", ".msi", ".pdb", ".pyc"}
TEST_PRIVATE_KEY = PurePosixPath("dock/src/companion/chat/fixtures/test-only-rsa.pem")
PRIVATE_KEY_HEADER = re.compile(r"-{5}BEGIN(?: [A-Z0-9]+)* PRIVATE KEY(?: BLOCK)?-{5}", re.I)
RUST_STRING = r'"(?:\\.|[^"\\])*"'
RUST_TOKEN = re.compile(
    rf'//[^\n]*|/\*.*?\*/|(?P<raw>r(?P<hashes>\#*)".*?"(?P=hashes))'
    rf'|(?P<string>{RUST_STRING})|(?P<name>\binclude(?:_bytes|_str)?\b)(?=\s*!)',
    re.S,
)


def excluded(path: PurePosixPath) -> bool:
    parts = tuple(part.lower() for part in path.parts)
    return (
        parts[0] in PRIVATE_DIRECTORIES
        or "/".join(parts) in PRIVATE_FILES
        or len(parts) == 1 and parts[0].endswith(("_done.md", "_review.md"))
        or any(part.startswith(".orca") for part in parts)
        or any(part in GENERATED_DIRECTORIES for part in parts[:-1])
        or path.suffix.lower() in GENERATED_SUFFIXES
    )


def tracked_files() -> list[PurePosixPath]:
    raw = subprocess.check_output(
        ["git", "ls-files", "--cached", "--full-name", "--deduplicate", "-z"],
        cwd=ROOT,
    )
    paths = [PurePosixPath(part) for part in raw.decode("utf-8").split("\0") if part]
    return [path for path in paths if not excluded(path)]


def private_patterns() -> list[tuple[int, re.Pattern[str]]]:
    override = os.environ.get("NOPBAR_EXPORT_PRIVATE")
    file = Path(override) if override else Path.home() / ".nopbar" / "export-private.txt"
    file = file.resolve()
    if file.is_relative_to(ROOT):
        raise ValueError("개인 패턴 파일은 저장소 밖에 있어야 합니다.")
    try:
        lines = file.read_text(encoding="utf-8-sig").splitlines()
    except FileNotFoundError:
        raise ValueError("개인 패턴 파일을 찾을 수 없습니다.") from None
    except UnicodeError:
        raise ValueError("개인 패턴 파일은 UTF-8이어야 합니다.") from None

    patterns = []
    for number, line in enumerate(lines, 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        try:
            patterns.append((number, re.compile(line, re.I)))
        except re.error:
            raise ValueError(f"개인 패턴 {number}번째 줄의 정규식이 잘못되었습니다.") from None
    if not patterns:
        raise ValueError("개인 패턴 파일에 정규식이 필요합니다.")
    return patterns


def check_files(paths: list[PurePosixPath]) -> list[str]:
    patterns = private_patterns()
    account = os.environ.get("USERNAME")
    sep = r"[/\\]+"
    local_path = (
        re.compile(rf"\b(?:users|home|workspaces|dev){sep}{re.escape(account)}(?=$|{sep}|[\s\x22\x27])", re.I)
        if account else None
    )
    problems = []
    for path in paths:
        data = (ROOT / path).read_bytes()
        variants = [path.as_posix(), data.decode("utf-8", errors="replace")]
        # Windows resources and some Unicode files store text in UTF-16.
        if b"\0" in data:
            variants.extend(data.decode(encoding, errors="replace") for encoding in ("utf-16-le", "utf-16-be"))
        if local_path and any(local_path.search(variant) for variant in variants):
            problems.append(f"{path}: 로컬 계정 경로")
        if path != TEST_PRIVATE_KEY and any(PRIVATE_KEY_HEADER.search(variant) for variant in variants):
            problems.append(f"{path}: 개인 키 머리말")
        for number, pattern in patterns:
            if any(pattern.search(variant) for variant in variants):
                problems.append(f"{path}: 개인 패턴 {number}번째 줄")
    return problems


def include_literal(token: re.Match[str]) -> str:
    if token.lastgroup == "raw":
        edge = len(token["hashes"]) + 1
        return token[0][edge + 1 : -edge]
    return json.loads(token[0])


def include_paths(path: PurePosixPath, source: str):
    """Find literal includes and concat! calls made of literal path segments."""
    for macro in RUST_TOKEN.finditer(source):
        if macro.lastgroup != "name":
            continue
        argument = re.match(r"\s*!\s*[(\[{]\s*(?:(concat)\s*!\s*\(\s*)?", source[macro.end():])
        if argument is None:
            continue
        rest = source[macro.end() + argument.end():]
        parts = []
        parent = path.parent
        manifest = None
        if argument[1]:
            manifest = re.match(r'env!\s*\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*,\s*', rest)
            if manifest:
                parent = next((p for p in path.parents if (ROOT / p / "Cargo.toml").is_file()), parent)
                rest = rest[manifest.end():]
        while token := RUST_TOKEN.match(rest):
            if token.lastgroup not in ("raw", "string"):
                break
            parts.append(include_literal(token))
            if not argument[1]:
                yield macro.start(), parent / parts[0]
                break
            rest = rest[token.end():].lstrip()
            comma = rest.startswith(",")
            if comma:
                rest = rest[1:].lstrip()
            if rest.startswith(")"):
                joined = "".join(parts)
                yield macro.start(), parent / (joined.lstrip("/\\") if manifest else joined)
                break
            if not comma:
                break


def check_includes(paths: list[PurePosixPath]) -> list[str]:
    public = {os.path.normcase(str(ROOT / path)) for path in paths}
    problems = []
    for path in paths:
        if path.suffix != ".rs":
            continue
        source = (ROOT / path).read_text(encoding="utf-8")
        for offset, target in include_paths(path, source):
            target_key = os.path.normcase(os.path.normpath(ROOT / target))
            if target_key not in public:
                line = source.count("\n", 0, offset) + 1
                problems.append(f"{path}:{line}: Rust include is missing from the export: {target}")
    return problems


def export(destination: Path, check: bool) -> int:
    destination = destination.resolve()
    if destination == ROOT or destination.is_relative_to(ROOT):
        raise ValueError("choose a destination outside the source worktree")
    if destination.exists() and (not destination.is_dir() or any(destination.iterdir())):
        raise ValueError("destination must be empty or absent")

    paths = tracked_files()
    for path in paths:
        source = ROOT / path
        if source.resolve() != source or not source.is_file():
            raise ValueError(f"expected a regular tracked file: {path}")
    if check:
        problems = check_files(paths) or check_includes(paths)
        if problems:
            for problem in problems:
                print(problem, file=sys.stderr)
            return 1

    destination.mkdir(parents=True, exist_ok=True)
    for path in paths:
        output = destination / path
        output.parent.mkdir(parents=True, exist_ok=True)
        with output.open("xb") as handle:
            handle.write((ROOT / path).read_bytes())

    status = "Exported and checked" if check else "Exported"
    print(f"{status} {len(paths)} files to {destination}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path, help="empty or absent directory outside the worktree")
    parser.add_argument("--check", action="store_true", help="check private traces using NOPBAR_EXPORT_PRIVATE or ~/.nopbar/export-private.txt")
    args = parser.parse_args()
    try:
        return export(args.destination, args.check)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
