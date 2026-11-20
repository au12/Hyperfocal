"""HTTP API of Hyperfocal ML.

    GET  /health        200 "ok" once the models are loaded
    GET  /v1/info       device, models (with content ids) and limits
    POST /v1/analyze    body: an upright JPEG or PNG; query: faces=0, animals=0 to skip
                        either. Answers the faces and animals found (see pipeline.py).

Set HFML_TOKEN to require "Authorization: Bearer <token>" on /v1/ (for a service
reachable from more than the Docker network).
"""

from __future__ import annotations

import hmac
import json
import logging
import os
import signal
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

from . import __version__
from .pipeline import Config, Pipeline, env

log = logging.getLogger("hyperfocal_ml")

MAX_BODY = 64 << 20


class Service:
    def __init__(self) -> None:
        self.cfg = Config()
        self.pipeline = Pipeline(self.cfg)
        workers = int(env("HFML_WORKERS", "0")) or (2 if self.pipeline.device == "cuda" else 1)
        # Requests decode and prepare in parallel; this many run the models at once.
        self.lock = threading.BoundedSemaphore(workers)
        self.workers = workers
        self.token = env("HFML_TOKEN", "")
        self.started = time.time()
        self.analyzed = 0
        self.busy_ms = 0.0
        self.stats_lock = threading.Lock()

    def info(self) -> dict:
        p = self.pipeline
        return {
            "service": "hyperfocal-ml",
            "version": __version__,
            "device": p.device,
            "workers": self.workers,
            "models": p.models,
            "face_scales": list(self.cfg.face_scales),
            "thresholds": {"face": self.cfg.face_threshold, "animal": self.cfg.animal_threshold},
            "analyzed": self.analyzed,
            "uptime_s": round(time.time() - self.started),
            "avg_ms": round(self.busy_ms / self.analyzed, 1) if self.analyzed else None,
        }


def make_handler(svc: Service):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        server_version = f"hyperfocal-ml/{__version__}"

        def log_message(self, fmt: str, *args) -> None:  # quiet access log
            log.debug("%s %s", self.address_string(), fmt % args)

        def _send(self, code: int, body: bytes, ctype: str = "application/json") -> None:
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def _json(self, code: int, obj) -> None:
            self._send(code, json.dumps(obj, separators=(",", ":")).encode())

        def _authorized(self) -> bool:
            if not svc.token:
                return True
            got = self.headers.get("Authorization", "")
            return hmac.compare_digest(got.encode(), f"Bearer {svc.token}".encode())

        def do_GET(self) -> None:  # noqa: N802
            path = urlparse(self.path).path
            if path == "/health":
                self._send(200, b"ok", "text/plain")
            elif path == "/v1/info":
                if not self._authorized():
                    return self._json(401, {"error": "unauthorized"})
                self._json(200, svc.info())
            else:
                self._json(404, {"error": "not found"})

        def do_POST(self) -> None:  # noqa: N802
            u = urlparse(self.path)
            if u.path != "/v1/analyze":
                return self._json(404, {"error": "not found"})
            if not self._authorized():
                return self._json(401, {"error": "unauthorized"})
            try:
                n = int(self.headers.get("Content-Length", "0"))
            except ValueError:
                n = 0
            if n <= 0 or n > MAX_BODY:
                # Drain nothing: an unusable length means the connection cannot be reused.
                self.close_connection = True
                return self._json(413 if n > MAX_BODY else 400, {"error": "send the image as the request body (at most 64 MB)"})
            data = self.rfile.read(n)
            q = parse_qs(u.query)
            flag = lambda k: q.get(k, ["1"])[0] not in ("0", "false", "no")  # noqa: E731
            num = lambda k: float(q[k][0]) if k in q else None  # noqa: E731
            t = time.perf_counter()
            try:
                res = svc.pipeline.analyze(
                    data, faces=flag("faces"), animals=flag("animals"), face_thr=num("face_threshold"),
                    animal_thr=num("animal_threshold"), lock=svc.lock,
                )
            except ValueError as e:
                return self._json(422, {"error": str(e)})
            except Exception as e:  # a model failure: report it, keep serving
                log.exception("analysis failed")
                return self._json(500, {"error": f"analysis failed: {e}"})
            with svc.stats_lock:
                svc.analyzed += 1
                svc.busy_ms += (time.perf_counter() - t) * 1000
            self._json(200, res)

    return Handler


def main() -> int:
    logging.basicConfig(
        level=env("HFML_LOG", "info").upper(),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
        stream=sys.stdout,
    )
    port = int(env("HFML_PORT", "8090"))
    log.info("Hyperfocal ML %s starting (models in %s)", __version__, env("HFML_MODELS", "/models"))
    svc = Service()
    info = svc.info()
    log.info("device %s, %d at once; models: %s", info["device"], svc.workers,
             ", ".join(f"{k}={v['file']}" for k, v in info["models"].items()))
    httpd = ThreadingHTTPServer(("0.0.0.0", port), make_handler(svc))
    httpd.daemon_threads = True

    def stop(*_):
        log.info("shutting down")
        threading.Thread(target=httpd.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    log.info("listening on :%d", port)
    httpd.serve_forever()
    return 0


def healthcheck() -> int:
    import urllib.request

    port = int(env("HFML_PORT", "8090"))
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=5) as r:
            return 0 if r.status == 200 else 1
    except Exception:
        return 1


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "healthcheck":
        sys.exit(healthcheck())
    sys.exit(main())
