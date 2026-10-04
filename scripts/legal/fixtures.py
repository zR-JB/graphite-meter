"""A private checkout for each legal test, cleaned after all of its patches and assertions."""
import subprocess
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory


def git(directory: Path, *args: str) -> str:
    """Git's output in `directory`, committing as a fixture identity without signing."""
    return subprocess.run(['git', '-C', str(directory), '-c', 'user.name=fixture', '-c', 'user.email=fixture@example.invalid',
                           '-c', 'commit.gpgsign=false', *args], check=True, text=True, stdout=subprocess.PIPE).stdout.strip()


class CheckoutTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(TemporaryDirectory())).resolve()
