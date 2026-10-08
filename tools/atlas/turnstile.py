"""Cooperative GPU lease client for turnstile (see the gpu-turnstile service on
bifrost). Adapted from slam-nerf/webapp/turnstile.py.

A granted lease heartbeats on a background thread until released, otherwise
turnstile reaps it. If turnstile is unreachable we proceed without a lease.
Set TURNSTILE_API_KEY for a named priority; without it we queue as anonymous.
"""

from __future__ import annotations

import logging
import os
import threading
import time

import httpx

log = logging.getLogger("atlas.turnstile")

_HEARTBEAT_INTERVAL = 10.0  # well inside turnstile's ~30 s reap TTL


def _base() -> str:
    return os.environ.get("TURNSTILE_URL", "http://localhost:8080").rstrip("/")


def _api_key() -> str:
    return os.environ.get("TURNSTILE_API_KEY", "")


class Lease:
    """A granted GPU lease that heartbeats in the background until released."""

    def __init__(self, client: httpx.Client, base: str, headers: dict, lease_id: str):
        self._client = client
        self._base = base
        self._headers = headers
        self._id = lease_id
        self._revoke = threading.Event()
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._heartbeat_loop, daemon=True)
        self._thread.start()

    def _heartbeat_loop(self) -> None:
        while not self._stop.wait(_HEARTBEAT_INTERVAL):
            try:
                r = self._client.post(f"{self._base}/lease/{self._id}/heartbeat",
                                      headers=self._headers)
                r.raise_for_status()
                if r.json().get("revoke_requested"):
                    self._revoke.set()
            except Exception as exc:  # noqa: BLE001
                log.warning("turnstile heartbeat failed (%s)", exc)

    @property
    def revoke_requested(self) -> bool:
        return self._revoke.is_set()

    def release(self) -> None:
        self._stop.set()
        try:
            self._client.post(f"{self._base}/lease/{self._id}/release", headers=self._headers)
            log.info("released turnstile GPU lease %s", self._id)
        except Exception as exc:  # noqa: BLE001
            log.warning("turnstile release failed (%s)", exc)
        finally:
            self._client.close()


def acquire(est_seconds: int | None = None, poll_interval: float = 1.0) -> Lease | None:
    """Block until turnstile grants the GPU; return a held Lease, or None if
    turnstile is unconfigured/unreachable (build proceeds best-effort)."""
    base = _base()
    if not base:
        return None
    headers = {"Authorization": f"Bearer {_api_key()}"} if _api_key() else {}
    client = httpx.Client(timeout=30)
    body: dict = {}
    if est_seconds:
        body["est_seconds"] = int(est_seconds)
    try:
        r = client.post(f"{base}/lease", headers=headers, json=body)
        r.raise_for_status()
        data = r.json()
        lease_id = data["lease_id"]
        status = data.get("status")
    except Exception as exc:  # noqa: BLE001
        log.warning("turnstile unavailable (%s); proceeding without a lease", exc)
        client.close()
        return None

    waited = 0.0
    while status != "granted":
        time.sleep(poll_interval)
        waited += poll_interval
        try:
            r = client.get(f"{base}/lease/{lease_id}", headers=headers)
            r.raise_for_status()
            status = r.json().get("status")
        except Exception as exc:  # noqa: BLE001
            log.warning("turnstile poll failed (%s); proceeding without a lease", exc)
            client.close()
            return None
        if waited % 30 < poll_interval:
            log.info("waiting on turnstile GPU lease %s (%.0fs)", lease_id, waited)

    log.info("turnstile GPU lease %s granted after %.0fs", lease_id, waited)
    return Lease(client, base, headers, lease_id)
