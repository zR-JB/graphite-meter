"""A private checkout for each legal test, cleaned after all of its patches and assertions."""
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory


class CheckoutTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(TemporaryDirectory())).resolve()
