"""Drop cached Cargo builds of packages whose Cargo.lock entries changed, and of their dependents."""

from pathlib import Path
import shutil
import subprocess
import tomllib


RUST = Path(__file__).resolve().parents[2] / "rust"
TARGET = RUST / "target"
SNAPSHOT = TARGET / "cached-Cargo.lock"


def packages(lock: Path) -> dict[str, dict]:
    return {f"{entry['name']} {entry['version']}": entry for entry in tomllib.loads(lock.read_text())["package"]}


def main() -> None:
    lock = RUST / "Cargo.lock"
    if SNAPSHOT.exists():
        old, new = packages(SNAPSHOT), packages(lock)
        stale = {key.split()[0] for key in old.keys() | new.keys() if old.get(key) != new.get(key)}
        while dependents := {entry["name"] for entry in new.values() if entry["name"] not in stale
                             and any(name.split()[0] in stale for name in entry.get("dependencies", []))}:
            stale |= dependents
        if stale - {entry["name"] for entry in new.values()}:
            # cargo clean selects only locked packages; one that left the graph restarts the cache.
            shutil.rmtree(TARGET)
        elif stale:
            for profile in TARGET.iterdir():
                if (profile / ".fingerprint").is_dir():
                    subprocess.run(["cargo", "clean", "--locked", "--profile",
                                    "dev" if profile.name == "debug" else profile.name,
                                    *(f"--package={name}" for name in sorted(stale))], cwd=RUST, check=True)
    TARGET.mkdir(exist_ok=True)
    shutil.copyfile(lock, SNAPSHOT)


if __name__ == "__main__":
    main()
