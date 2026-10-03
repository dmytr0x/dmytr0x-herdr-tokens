import subprocess
import unittest
from unittest.mock import Mock
from harness_support import cleanup_all, terminate


class CleanupTests(unittest.TestCase):
    def test_failed_stop_does_not_skip_termination_or_close(self) -> None:
        events: list[str] = []
        def fail() -> None:
            events.append("stop")
            raise subprocess.TimeoutExpired("stop", 6)
        with self.assertRaises(ExceptionGroup):
            cleanup_all(fail, lambda: events.append("terminate"), lambda: events.append("close"))
        self.assertEqual(events, ["stop", "terminate", "close"])

    def test_term_timeout_always_kills_and_waits(self) -> None:
        process = Mock()
        process.wait.side_effect = [subprocess.TimeoutExpired("wait", 6), subprocess.TimeoutExpired("wait", 6), 0]
        terminate(process)
        process.terminate.assert_called_once()
        process.kill.assert_called_once()
        self.assertEqual(process.wait.call_count, 3)


if __name__ == "__main__":
    unittest.main()
