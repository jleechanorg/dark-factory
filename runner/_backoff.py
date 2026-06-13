"""Backoff-with-jitter retry decorator for transient-failure calls.

Goal
----
Many runner call sites touch external systems (subprocesses, sockets,
filesystems, LLM APIs) that fail transiently. The Dark Factory prefers
a small, well-tested helper over ad-hoc ``for _ in range(N): try: ...
except: sleep(...); continue`` loops scattered through the codebase.
This module is the single source of truth for that pattern.

API
---
- :func:`backoff`  — decorator that retries a function on a set of
  declared transient exception types, with exponential backoff and
  optional full-jitter.
- :func:`compute_delay`  — pure function: given attempt index, base,
  cap, and a [0, 1) jitter fraction, return the seconds to sleep.

Backoff curve
-------------
- Attempt 0 (the first call): no sleep before.
- Attempt n (n >= 1): sleep for ``min(base * 2**(n-1), cap)`` seconds,
  then apply full jitter by sampling ``Uniform(0, sleep)`` (the AWS
  Architecture Blog "exponential backoff and jitter" recommendation).
  Full jitter avoids thundering-herd when many callers retry in
  lockstep after a shared dependency hiccup.

Error model
-----------
The decorated function MUST raise one of the declared ``on``
exception types to trigger a retry. Any other exception propagates
immediately without retry — the decorator is for transient failures,
not for swallowing bugs. The final attempt's exception is re-raised
after all retries are exhausted.

Example
-------
::

    @backoff(retries=3, base=0.1, cap=2.0, on=(ConnectionError, TimeoutError))
    def fetch_metadata():
        return httpx.get(\"https://example.com/meta\", timeout=5)
"""

from __future__ import annotations

import functools
import random
import time
from typing import Callable, Optional, Tuple, Type, TypeVar, cast

T = TypeVar("T")


def compute_delay(
    attempt: int,
    base: float,
    cap: float,
    *,
    jitter: float = 1.0,
    rng: Optional[random.Random] = None,
) -> float:
    """Return the seconds to sleep before retry ``attempt``.

    Parameters
    ----------
    attempt:
        1-based attempt index — attempt=1 is the first sleep before
        retrying the original call. ``attempt <= 0`` returns 0.
    base:
        Base sleep in seconds. The first sleep is ``base * 2**0``;
        the second is ``base * 2**1``; etc.
    cap:
        Maximum sleep in seconds. Sleep never exceeds this value
        regardless of attempt.
    jitter:
        Fraction in [0, 1] of the computed sleep to use. ``1.0`` is
        full jitter (uniform random in [0, sleep]); ``0.0`` is no
        jitter (deterministic). Other values are a fixed fraction of
        the unjittered sleep.
    rng:
        Optional ``random.Random`` for deterministic test outcomes.
        Defaults to the module-level ``random`` instance.

    Returns
    -------
    Seconds to sleep. Always non-negative.
    """
    if attempt <= 0:
        return 0.0
    raw = base * (2 ** (attempt - 1))
    bounded = min(raw, cap)
    if jitter <= 0.0:
        return bounded
    sample_at = rng.random() if rng is not None else random.random()
    return bounded * min(max(jitter * sample_at, 0.0), 1.0)


def backoff(
    *,
    retries: int = 3,
    base: float = 0.1,
    cap: float = 10.0,
    jitter: float = 1.0,
    on: Tuple[Type[BaseException], ...] = (Exception,),
    sleep: Callable[[float], None] = time.sleep,
    rng: Optional[random.Random] = None,
) -> Callable[[Callable[..., T]], Callable[..., T]]:
    """Decorate ``fn`` to retry on transient failures with backoff.

    Parameters
    ----------
    retries:
        Maximum number of retries after the first attempt. Total
        call count is ``1 + retries``. ``retries=0`` is a no-op
        decorator — the function is called once with no sleep.
    base, cap, jitter:
        Forwarded to :func:`compute_delay` for each retry's sleep.
    on:
        Tuple of exception types that trigger a retry. Any other
        exception type propagates immediately without retry.
    sleep:
        Callable used to sleep. Defaults to ``time.sleep``. Tests
        pass a list-appending callable to assert on the call
        sequence without actually sleeping.
    rng:
        Optional ``random.Random`` for deterministic test outcomes.

    Returns
    -------
    A decorator that wraps the target function with the retry
    behavior.
    """

    if retries < 0:
        raise ValueError("retries must be >= 0")
    if base <= 0:
        raise ValueError("base must be > 0")
    if cap < base:
        raise ValueError("cap must be >= base")
    if not 0.0 <= jitter <= 1.0:
        raise ValueError("jitter must be in [0, 1]")
    if not on:
        raise ValueError("on must be a non-empty tuple of exception types")

    def decorator(fn: Callable[..., T]) -> Callable[..., T]:
        @functools.wraps(fn)
        def wrapper(*args: object, **kwargs: object) -> T:
            last_exc: Optional[BaseException] = None
            for attempt in range(retries + 1):
                try:
                    return fn(*args, **kwargs)
                except on as exc:
                    last_exc = exc
                    if attempt >= retries:
                        break
                    sleep(compute_delay(attempt + 1, base, cap, jitter=jitter, rng=rng))
            assert last_exc is not None  # only reachable via except
            raise last_exc

        return cast(Callable[..., T], wrapper)

    return decorator
