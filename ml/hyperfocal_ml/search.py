"""Search by content: SigLIP 2 (Google, Apache-2.0) image and text embeddings.

The image and the text model map into the same 768-dimensional space: a photo and a
description of it lie close together (the cosine of a match is small in absolute terms,
about 0.10 to 0.20; SigLIP's calibration turns it into a probability), and two photos
of the same scene lie very close (about 0.85 and up).

Images are squashed to 224 x 224 (no crop: SigLIP's own preprocessing), pixel values
scaled to -1..1. Text is lower-cased, ends with <eos> and is padded to 64 tokens, as
SigLIP 2 was trained.
"""

from __future__ import annotations

import logging
import threading
import time
from pathlib import Path

import cv2
import numpy as np
import onnxruntime as ort

log = logging.getLogger("hyperfocal_ml")

SIZE = 224
TOKENS = 64
DIM = 768
# The model's learned calibration: p(match) = sigmoid(cos * SCALE + BIAS).
LOGIT_SCALE = 112.66891
LOGIT_BIAS = -16.771725

VISION = "siglip2-vision.onnx"
TEXT = "siglip2-text.onnx"
TOKENIZER = "siglip2-tokenizer.json"


def available(models_dir: Path) -> bool:
    return all((models_dir / f).exists() for f in (VISION, TEXT, TOKENIZER))


class Siglip:
    def __init__(self, models_dir: Path, session) -> None:
        """`session(path)` makes an ONNX Runtime session the way the pipeline does."""
        from tokenizers import Tokenizer

        self.vision = session(models_dir / VISION)
        self.vision_input = self.vision.get_inputs()[0].name
        out = [o.name for o in self.vision.get_outputs()]
        self.vision_output = "pooler_output" if "pooler_output" in out else out[-1]
        self._text_path = models_dir / TEXT
        self._session = session
        self._text: ort.InferenceSession | None = None
        self._text_lock = threading.Lock()
        tok = Tokenizer.from_file(str(models_dir / TOKENIZER))
        tok.enable_truncation(max_length=TOKENS)
        pad = tok.token_to_id("<pad>")
        tok.enable_padding(length=TOKENS, pad_id=0 if pad is None else pad, pad_token="<pad>")
        self.tokenizer = tok

    def _text_session(self) -> ort.InferenceSession:
        # The text model is large (a 256,000-word vocabulary): loaded on the first query.
        with self._text_lock:
            if self._text is None:
                t = time.time()
                self._text = self._session(self._text_path)
                log.info("text model loaded in %.1fs", time.time() - t)
            return self._text

    @staticmethod
    def prepare(rgb: np.ndarray) -> np.ndarray:
        h, w = rgb.shape[:2]
        interp = cv2.INTER_AREA if min(h, w) > SIZE else cv2.INTER_CUBIC
        x = cv2.resize(rgb, (SIZE, SIZE), interpolation=interp).astype(np.float32)
        return (x / 127.5 - 1.0).transpose(2, 0, 1)

    def embed_images(self, rgbs: list[np.ndarray]) -> np.ndarray:
        x = np.stack([self.prepare(r) for r in rgbs])
        out = []
        for i in range(0, len(x), 16):
            out.append(self.vision.run([self.vision_output], {self.vision_input: np.ascontiguousarray(x[i : i + 16])})[0])
        e = np.concatenate(out).astype(np.float32)
        return e / np.maximum(np.linalg.norm(e, axis=1, keepdims=True), 1e-9)

    def tokens(self, texts: list[str]) -> np.ndarray:
        enc = self.tokenizer.encode_batch([t.strip().lower() for t in texts])
        return np.array([e.ids for e in enc], dtype=np.int64)

    def embed_texts(self, texts: list[str]) -> np.ndarray:
        sess = self._text_session()
        ids = self.tokens(texts)
        name = sess.get_inputs()[0].name
        outs = [o.name for o in sess.get_outputs()]
        e = sess.run(["pooler_output" if "pooler_output" in outs else outs[-1]], {name: ids})[0].astype(np.float32)
        return e / np.maximum(np.linalg.norm(e, axis=1, keepdims=True), 1e-9)
