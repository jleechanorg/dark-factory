"""Tests for :mod:`runner._backoff`."""

from __future__ import annotations

import random

import pytest

from runner._backoff import backoff, compute_delay


# ---------------------------------------------------------------------------
# compute_delay — pure function
# ---------------------------------------------------------------------------


def test_compute_delay_zero_attempt_is_zero():
    """No sleep is needed before the first call (attempt <= 0)."""
    assert compute_delay(0, base=0.1, cap=10.0) == 0.0
    assert compute_delay(-1, base=0.1, cap=10.0) == 0.0


def test_compute_delay_doubles_each_attempt_without_cap():
    """With no jitter and no cap, delay doubles each attempt."""
    assert compute_delay(1, base=0.1, cap=10.0, jitter=0.0) == 0.1
    assert compute_delay(2, base=0.1, cap=10.0, jitter=0.0) == 0.2
    assert compute_delay(3, base=0.1, cap=10.0, jitter=0.0) == 0.4
    assert compute_delay(4, base=0.1, cap=10.0, jitter=0.0) == 0.8


def test_compute_delay_caps_at_cap():
    """Once base * 2**(n-1) exceeds cap, the cap applies."""
    assert compute_delay(20, base=0.1, cap=2.0, jitter=0.0) == 2.0
    assert compute_delay(100, base=0.1, cap=2.0, jitter=0.0) == 2.0


def test_compute_delay_full_jitter_is_bounded():
    """With full jitter, the delay is in [0, base * 2**(n-1)]."""
    rng = random.Random(0)
    for _ in range(50):
        d = compute_delay(3, base=0.1, cap=10.0, jitter=1.0, rng=rng)
        assert 0.0 <= d <= 0.4  # base * 2**2


def test_compute_delay_deterministic_with_seed():
    """Seeded RNG produces the same sequence across calls."""
    rng_a = random.Random(42)
    rng_b = random.Random(42)
    a = [compute_delay(2, base=0.1, cap=10.0, jitter=1.0, rng=rng_a) for _ in range(10)]
    b = [compute_delay(2, base=0.1, cap=10.0, jitter=1.0, rng=rng_b) for _ in range(10)]
    assert a == b


def test_compute_delay_zero_jitter_is_deterministic():
    """``jitter=0`` ignores the RNG and returns the bounded delay."""
    rng = random.Random(0)
    d = compute_delay(5, base=0.1, cap=10.0, jitter=0.0, rng=rng)
    assert d == 1.6  # 0.1 * 2**4


# ---------------------------------------------------------------------------
# backoff — happy path
# ---------------------------------------------------------------------------


def test_backoff_no_retry_when_not_needed():
    """A function that succeeds on the first call is called exactly once."""

    calls = []

    @backoff(retries=3, base=0.1, cap=1.0, on=(ValueError,))
    def fn() -> str:
        calls.append("call")
        return "ok"

    assert fn() == "ok"
    assert calls == ["call"]


def test_backoff_retries_until_success():
    """A function that fails N-1 times then succeeds is retried."""

    calls = []

    def fn() -> str:
        calls.append("call")
        if len(calls) < 3:
            raise ValueError("transient")
        return "ok"

    sleeps: list[float] = []

    def fake_sleep(s: float) -> None:
        sleeps.append(s)

    decorated = backoff(
        retries=5, base=0.01, cap=0.1, on=(ValueError,), sleep=fake_sleep
    )(fn)
    assert decorated() == "ok"
    assert len(calls) == 3
    assert len(sleeps) == 2  # one sleep per failed attempt, none before the first call


def test_backoff_raises_after_exhausted_retries():
    """After retries are exhausted, the last exception is re-raised."""

    calls = []

    def fn() -> str:
        calls.append("call")
        raise ValueError("always fails")

    sleeps: list[float] = []

    def fake_sleep(s: float) -> None:
        sleeps.append(s)

    decorated = backoff(
        retries=2, base=0.01, cap=0.1, on=(ValueError,), sleep=fake_sleep
    )(fn)
    with pytest.raises(ValueError, match="always fails"):
        decorated()
    assert len(calls) == 3  # initial + 2 retries
    assert len(sleeps) == 2  # one sleep per retry


def test_backoff_does_not_retry_undeclared_exception():
    """An exception not in the ``on`` tuple propagates immediately."""

    calls = []

    @backoff(retries=5, base=0.01, cap=0.1, on=(ValueError,))
    def fn() -> str:
        calls.append("call")
        raise RuntimeError("not retryable")

    with pytest.raises(RuntimeError, match="not retryable"):
        fn()
    assert calls == ["call"]  # no retry on the first failure


def test_backoff_sleep_sequence_is_exponential():
    """Sleep values follow the exponential backoff curve."""

    def fn() -> str:
        raise ValueError("fail")

    sleeps: list[float] = []

    def fake_sleep(s: float) -> None:
        sleeps.append(s)

    decorated = backoff(
        retries=4, base=0.1, cap=10.0, on=(ValueError,), sleep=fake_sleep, jitter=0.0
    )(fn)
    with pytest.raises(ValueError):
        decorated()
    # 4 sleeps: base * 2**0, 2**1, 2**2, 2**3 = 0.1, 0.2, 0.4, 0.8
    assert sleeps == pytest.approx([0.1, 0.2, 0.4, 0.8])


def test_backoff_sleep_respects_cap():
    """Once exponential growth exceeds cap, the cap is applied."""

    def fn() -> str:
        raise ValueError("fail")

    sleeps: list[float] = []

    def fake_sleep(s: float) -> None:
        sleeps.append(s)

    decorated = backoff(
        retries=5, base=0.1, cap=0.3, on=(ValueError,), sleep=fake_sleep, jitter=0.0
    )(fn)
    with pytest.raises(ValueError):
        decorated()
    # Without cap: 0.1, 0.2, 0.4, 0.8, 1.6
    # With cap=0.3: 0.1, 0.2, 0.3, 0.3, 0.3
    assert sleeps == pytest.approx([0.1, 0.2, 0.3, 0.3, 0.3])


# ---------------------------------------------------------------------------
# backoff — configuration validation
# ---------------------------------------------------------------------------


def test_backoff_retries_zero_means_one_attempt():
    """``retries=0`` calls the function exactly once with no retry."""

    calls = []

    @backoff(retries=0, base=0.1, cap=1.0, on=(ValueError,))
    def fn() -> str:
        calls.append("call")
        raise ValueError("fail")

    with pytest.raises(ValueError):
        fn()
    assert calls == ["call"]


@pytest.mark.parametrize("bad_value", [-1, -100])
def test_backoff_negative_retries_raises(bad_value):
    with pytest.raises(ValueError, match="retries"):
        backoff(retries=bad_value, base=0.1, cap=1.0, on=(ValueError,))


@pytest.mark.parametrize("bad_value", [0.0, -0.1, -1.0])
def test_backoff_non_positive_base_raises(bad_value):
    with pytest.raises(ValueError, match="base"):
        backoff(retries=1, base=bad_value, cap=1.0, on=(ValueError,))


def test_backoff_cap_smaller_than_base_raises():
    with pytest.raises(ValueError, match="cap"):
        backoff(retries=1, base=1.0, cap=0.5, on=(ValueError,))


@pytest.mark.parametrize("bad_value", [-0.1, 1.1, 2.0])
def test_backoff_jitter_out_of_range_raises(bad_value):
    with pytest.raises(ValueError, match="jitter"):
        backoff(retries=1, base=0.1, cap=1.0, on=(ValueError,), jitter=bad_value)


def test_backoff_empty_on_tuple_raises():
    with pytest.raises(ValueError, match="on"):
        backoff(retries=1, base=0.1, cap=1.0, on=())


# ---------------------------------------------------------------------------
# backoff — multi-exception ``on`` tuple
# ---------------------------------------------------------------------------


def test_backoff_retries_any_declared_exception():
    """All declared exception types trigger a retry."""

    # Closure-tracked state: raise ValueError on the first call,
    # KeyError on the second, return "ok" on the third.
    state = {"count": 0}

    def fn() -> str:
        state["count"] += 1
        if state["count"] == 1:
            raise ValueError("v")
        if state["count"] == 2:
            raise KeyError("k")
        return "ok"

    sleeps: list[float] = []

    def fake_sleep(s: float) -> None:
        sleeps.append(s)

    decorated = backoff(
        retries=2, base=0.01, cap=0.1, on=(ValueError, KeyError), sleep=fake_sleep
    )(fn)
    assert decorated() == "ok"
    assert state["count"] == 3
    assert len(sleeps) == 2


# ---------------------------------------------------------------------------
# backoff — full jitter (the realistic case)
# ---------------------------------------------------------------------------


def test_backoff_full_jitter_produces_in_range_delays():
    """With full jitter, all observed sleeps are within the bounded range."""
    rng = random.Random(0)

    def fn() -> str:
        raise ValueError("fail")

    sleeps: list[float] = []

    def fake_sleep(s: float) -> None:
        sleeps.append(s)

    decorated = backoff(
        retries=10, base=0.1, cap=2.0, on=(ValueError,), sleep=fake_sleep, rng=rng
    )(fn)
    with pytest.raises(ValueError):
        decorated()
    # Without cap violation: max sleep is min(0.1 * 2**n, 2.0).
    # For n=0..9: 0.1, 0.2, 0.4, 0.8, 1.6, 2.0, 2.0, 2.0, 2.0, 2.0
    expected_maxes = [0.1, 0.2, 0.4, 0.8, 1.6, 2.0, 2.0, 2.0, 2.0, 2.0]
    for actual, expected_max in zip(sleeps, expected_maxes):
        assert 0.0 <= actual <= expected_max
