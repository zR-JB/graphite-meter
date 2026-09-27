"""Bound the production dependency graphs of the experimental Rust binaries."""

from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[2]


def main() -> None:
    exceeded = False
    for package, target, limit in (("graphite-meter-server", "x86_64-unknown-linux-gnu", 135),
                                   ("graphite-meter-client", "x86_64-unknown-linux-musl", 164)):
        tree = subprocess.run(
            [
                "cargo", "tree", "--locked", "--edges", "normal",
                "--target", target, "--prefix", "none",
                "--format", "{p}", "--package", package,
            ],
            cwd=ROOT / "rust",
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        ).stdout
        packages = {line.split(" (", 1)[0] for line in tree.splitlines() if line}
        print(f"{package}: {len(packages)}/{limit} production crates (root included)")
        exceeded |= len(packages) > limit
    if exceeded:
        raise SystemExit("Rust dependency budget exceeded")


if __name__ == "__main__":
    main()
