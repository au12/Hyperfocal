"""Tests of the ML service.

The geometry tests always run. The model tests run when the models are present
(HFML_MODELS, as in the image: `python -m pytest ml/tests` inside the container, or
point HFML_MODELS at a folder made with fetch_models.py).
"""

import base64
import json
import os
import threading
import urllib.request
from pathlib import Path

import cv2
import numpy as np
import pytest

from hyperfocal_ml.pipeline import ARCFACE_DST, b64_f16, nms, umeyama

MODELS = Path(os.environ.get("HFML_MODELS", "/models"))
HAVE_MODELS = all((MODELS / f).exists() for f in ("yunet.onnx", "auraface.onnx", "pet-recognition.onnx"))


def test_umeyama_recovers_a_similarity():
    a = np.deg2rad(23.0)
    rot = np.array([[np.cos(a), -np.sin(a)], [np.sin(a), np.cos(a)]])
    src = ARCFACE_DST.astype(np.float64)
    dst = 1.7 * src @ rot.T + np.array([12.0, -5.0])
    m = umeyama(src, dst)
    back = src @ m[:, :2].T + m[:, 2]
    assert np.allclose(back, dst, atol=1e-6)
    assert np.isclose(np.sqrt(abs(np.linalg.det(m[:, :2]))), 1.7)


def test_nms_keeps_the_best_of_overlaps():
    boxes = np.array([[0, 0, 10, 10], [1, 1, 10, 10], [50, 50, 10, 10]], dtype=np.float32)
    scores = np.array([0.8, 0.9, 0.7], dtype=np.float32)
    assert nms(boxes, scores, 0.3) == [1, 2]
    assert nms(np.zeros((0, 4), np.float32), np.zeros(0, np.float32), 0.3) == []


def test_embeddings_are_little_endian_f16():
    v = np.array([0.5, -0.25, 1.0], dtype=np.float32)
    raw = base64.b64decode(b64_f16(v))
    assert np.array_equal(np.frombuffer(raw, "<f2").astype(np.float32), v)


def _synthetic_face_free_image() -> bytes:
    img = np.full((480, 640, 3), 200, np.uint8)
    cv2.rectangle(img, (100, 100), (300, 300), (40, 90, 160), -1)
    ok, buf = cv2.imencode(".jpg", img)
    assert ok
    return buf.tobytes()


@pytest.mark.skipif(not HAVE_MODELS, reason="models not available")
def test_pipeline_answers_in_normalized_coordinates():
    from hyperfocal_ml.pipeline import Pipeline

    p = Pipeline()
    out = p.analyze(_synthetic_face_free_image())
    assert out["w"] == 640 and out["h"] == 480
    assert out["faces"] == [] and out["animals"] == []


@pytest.mark.skipif(not HAVE_MODELS, reason="models not available")
def test_http_service_round_trip():
    from http.server import ThreadingHTTPServer

    from hyperfocal_ml.server import Service, make_handler

    svc = Service()
    srv = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(svc))
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    try:
        base = f"http://127.0.0.1:{srv.server_address[1]}"
        info = json.load(urllib.request.urlopen(base + "/v1/info"))
        assert info["service"] == "hyperfocal-ml"
        assert info["models"]["face_embedder"]["dim"] == 512
        req = urllib.request.Request(base + "/v1/analyze", data=_synthetic_face_free_image(), headers={"Content-Type": "image/jpeg"})
        out = json.load(urllib.request.urlopen(req))
        assert out["faces"] == []
        bad = urllib.request.Request(base + "/v1/analyze", data=b"not an image", headers={"Content-Type": "image/jpeg"})
        with pytest.raises(urllib.error.HTTPError) as e:
            urllib.request.urlopen(bad)
        assert e.value.code in (400, 415, 422)
    finally:
        srv.shutdown()
