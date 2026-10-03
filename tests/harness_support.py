"""Cleanup primitives shared by the isolated acceptance and soak harnesses."""
from collections.abc import Callable
import os
import signal
import subprocess


def cleanup_all(*actions: Callable[[], object]) -> None:
    failures: list[Exception] = []
    for action in actions:
        try:
            action()
        except Exception as error:
            failures.append(error)
    if failures:
        raise ExceptionGroup("harness cleanup failed", failures)


def terminate(process: subprocess.Popen, *, group: bool = False) -> None:
    try:
        process.wait(timeout=6)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        if group:
            os.killpg(process.pid, signal.SIGTERM)
        else:
            process.terminate()
        process.wait(timeout=6)
    except subprocess.TimeoutExpired:
        if group:
            os.killpg(process.pid, signal.SIGKILL)
        else:
            process.kill()
    except ProcessLookupError:
        pass
    finally:
        process.wait(timeout=6)
