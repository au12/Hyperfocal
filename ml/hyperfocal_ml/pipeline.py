"""The detection and recognition pipeline.

Faces:  YuNet (OpenCV Zoo, MIT) at three scales, so a face from about 0.6% to 95% of
        the long edge is found, then alignment on its five landmarks and an AuraFace
        (Apache-2.0) embedding from the full-resolution pixels.
Pets:   RF-DETR (Roboflow, Apache-2.0) for dogs and cats, then a pet re-identification
        embedding (open-noodle pet-recognition, Apache-2.0) of each animal.
Search: SigLIP 2 (Google, Apache-2.0) image and text embeddings in one space (search.py).
Eyes:   MediaPipe Face Landmarker (Google, Apache-2.0) on close-ups of known faces: open
        or closed (eyes.py).

Every coordinate in the answer is normalized to the image that was sent (0..1, origin
top left), which must be upright: EXIF orientation is ignored.
"""

from __future__ import annotations

import base64
import hashlib
import logging
import math
import os
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

import cv2
import numpy as np
import onnxruntime as ort

from . import eyes, search

log = logging.getLogger("hyperfocal_ml")

# ArcFace's 112x112 template: eyes, nose tip, mouth corners (image left to right).
ARCFACE_DST = np.array(
    [[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]],
    dtype=np.float32,
)

IMAGENET_MEAN = np.array([0.485, 0.456, 0.406], dtype=np.float32)
IMAGENET_STD = np.array([0.229, 0.224, 0.225], dtype=np.float32)

# RF-DETR's classes are COCO's 91-id space. The best class is taken among the animals a
# household library holds, so a horse or a cow wins its own argmax (and is dropped)
# instead of being filed as its nearest dog; only cats and dogs are kept.
COCO_ANIMALS = np.array([16, 17, 18, 19, 20, 21], dtype=np.int64)  # bird cat dog horse sheep cow
PETS = {17: "cat", 18: "dog"}


def env(name: str, default: str) -> str:
    v = os.environ.get(name, "").strip()
    return v or default


def fingerprint(path: Path) -> str:
    """A short content hash of a model file (identifies the embedding space)."""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(4 << 20):
            h.update(chunk)
    return h.hexdigest()[:12]


def umeyama(src: np.ndarray, dst: np.ndarray) -> np.ndarray:
    """Least-squares similarity transform (rotation, uniform scale, translation) that
    maps `src` points onto `dst` (Umeyama 1991), as a 2x3 matrix."""
    src = src.astype(np.float64)
    dst = dst.astype(np.float64)
    n = src.shape[0]
    mu_s, mu_d = src.mean(0), dst.mean(0)
    s0, d0 = src - mu_s, dst - mu_d
    cov = d0.T @ s0 / n
    u, sig, vt = np.linalg.svd(cov)
    d = np.ones(2)
    if np.linalg.det(cov) < 0:
        d[-1] = -1
    r = u @ np.diag(d) @ vt
    var = (s0**2).sum() / n
    scale = (sig * d).sum() / var if var > 0 else 1.0
    t = mu_d - scale * r @ mu_s
    m = np.zeros((2, 3))
    m[:, :2] = scale * r
    m[:, 2] = t
    return m


def nms(boxes: np.ndarray, scores: np.ndarray, iou: float) -> list[int]:
    """Greedy non-maximum suppression on x, y, w, h boxes. Returns kept indices."""
    if len(boxes) == 0:
        return []
    x1, y1 = boxes[:, 0], boxes[:, 1]
    x2, y2 = x1 + boxes[:, 2], y1 + boxes[:, 3]
    area = np.maximum(boxes[:, 2], 0) * np.maximum(boxes[:, 3], 0)
    order = np.argsort(-scores)
    keep: list[int] = []
    while order.size:
        i = int(order[0])
        keep.append(i)
        rest = order[1:]
        xx1 = np.maximum(x1[i], x1[rest])
        yy1 = np.maximum(y1[i], y1[rest])
        xx2 = np.minimum(x2[i], x2[rest])
        yy2 = np.minimum(y2[i], y2[rest])
        inter = np.maximum(0, xx2 - xx1) * np.maximum(0, yy2 - yy1)
        o = inter / np.maximum(area[i] + area[rest] - inter, 1e-9)
        order = rest[o <= iou]
    return keep


def b64_f16(v: np.ndarray) -> str:
    return base64.b64encode(v.astype("<f2").tobytes()).decode("ascii")


@dataclass
class Config:
    models_dir: Path = field(default_factory=lambda: Path(env("HFML_MODELS", "/models")))
    device: str = field(default_factory=lambda: env("HFML_DEVICE", "auto").lower())
    threads: int = field(default_factory=lambda: int(env("HFML_THREADS", "0")))
    face_det: str = field(default_factory=lambda: env("HFML_FACE_DETECTOR", "yunet.onnx"))
    face_emb: str = field(default_factory=lambda: env("HFML_FACE_EMBEDDER", "auraface.onnx"))
    animal_det: str = field(default_factory=lambda: env("HFML_ANIMAL_DETECTOR", "auto"))
    pet_emb: str = field(default_factory=lambda: env("HFML_PET_EMBEDDER", "pet-recognition.onnx"))
    # Long edges the face detector runs at (each a pass); YuNet finds faces of about
    # 10 to 300 pixels in a pass.
    face_scales: tuple[int, ...] = field(
        default_factory=lambda: tuple(int(s) for s in env("HFML_FACE_SCALES", "1600,640,320").split(",") if s.strip())
    )
    face_threshold: float = field(default_factory=lambda: float(env("HFML_FACE_THRESHOLD", "0.7")))
    # Embedding lengths that map to face quality 0 and 1 (AuraFace's scale; another
    # embedder may need its own).
    face_norm_low: float = field(default_factory=lambda: float(env("HFML_FACE_NORM_LOW", "12")))
    face_norm_high: float = field(default_factory=lambda: float(env("HFML_FACE_NORM_HIGH", "24")))
    animal_threshold: float = field(default_factory=lambda: float(env("HFML_ANIMAL_THRESHOLD", "0.35")))
    # Search by content (SigLIP 2): on when its models are there, unless HFML_SEARCH=0.
    search: bool = field(default_factory=lambda: env("HFML_SEARCH", "1").lower() not in ("0", "false", "no", "off"))
    # Open or closed eyes (MediaPipe Face Landmarker): on when its models are there,
    # unless HFML_EYES=0.
    eyes: bool = field(default_factory=lambda: env("HFML_EYES", "1").lower() not in ("0", "false", "no", "off"))


class Pipeline:
    def __init__(self, cfg: Config | None = None):
        self.cfg = cfg or Config()
        self.device = "cpu"
        self.providers = self._providers()
        d = self.cfg.models_dir
        self.face_det = self._session(d / self.cfg.face_det)
        self.face_emb = self._session(d / self.cfg.face_emb)
        animal = self.cfg.animal_det
        if animal == "auto":
            # Small (512 px, COCO AP 53) finds more small and half-hidden pets than Nano
            # (384 px, AP 48) for about 0.3 s more a photo on a CPU: worth it for a
            # one-time pass. HFML_ANIMAL_DETECTOR=rfdetr-nano.onnx for a weak CPU.
            animal = next((a for a in ("rfdetr-small.onnx", "rfdetr-nano.onnx", "rfdetr-medium.onnx") if (d / a).exists()), "rfdetr-small.onnx")
        self.animal_det_name = animal
        self.animal_det = self._session(d / animal)
        self.pet_emb = self._session(d / self.cfg.pet_emb)
        self.face_emb_input = self.face_emb.get_inputs()[0].name
        self.pet_emb_input = self.pet_emb.get_inputs()[0].name
        self.animal_input = self.animal_det.get_inputs()[0].name
        self.animal_size = int(self.animal_det.get_inputs()[0].shape[2])
        self.models = {
            "face_detector": {"file": self.cfg.face_det, "id": fingerprint(d / self.cfg.face_det)},
            "face_embedder": {"file": self.cfg.face_emb, "id": fingerprint(d / self.cfg.face_emb), "dim": 512},
            "animal_detector": {"file": animal, "id": fingerprint(d / animal), "size": self.animal_size},
            "pet_embedder": {"file": self.cfg.pet_emb, "id": fingerprint(d / self.cfg.pet_emb), "dim": 512},
        }
        self.siglip = None
        if self.cfg.search and search.available(d):
            self.siglip = search.Siglip(d, self._session)
            self.models["image_embedder"] = {"file": search.VISION, "id": fingerprint(d / search.VISION), "dim": search.DIM}
            self.models["text_embedder"] = {"file": search.TEXT, "id": fingerprint(d / search.TEXT), "dim": search.DIM}
        self.eyes = None
        if self.cfg.eyes and eyes.available(d):
            self.eyes = eyes.Eyes(d, self._session)
            self.models["eye_checker"] = {
                "file": eyes.LANDMARKS,
                "id": fingerprint(d / eyes.LANDMARKS) + fingerprint(d / eyes.BLENDSHAPES)[:4],
                "crop": eyes.CROP_SIDE,
            }
        self._warm()

    # ------------------------------------------------------------------ runtime

    def _providers(self) -> list:
        want = self.cfg.device
        avail = ort.get_available_providers()
        if want in ("auto", "cuda") and "CUDAExecutionProvider" in avail:
            # CUDA and cuDNN come from NVIDIA's pip wheels in the image.
            if hasattr(ort, "preload_dlls"):
                try:
                    ort.preload_dlls()
                except Exception as e:  # pragma: no cover - depends on the host
                    log.warning("preloading the CUDA libraries failed: %s", e)
            self.device = "cuda"
            opts = {
                "device_id": int(env("HFML_GPU", "0")),
                # Grow the arena by what a run needs, not by doubling: a 4 GB card is
                # often shared with other apps.
                "arena_extend_strategy": "kSameAsRequested",
                # The face detector sees a few image shapes; heuristic algorithm choice
                # avoids a slow cuDNN benchmark for each new one.
                "cudnn_conv_algo_search": "HEURISTIC",
            }
            limit = env("HFML_GPU_MEM_MB", "")
            if limit:
                opts["gpu_mem_limit"] = int(limit) << 20
            return [("CUDAExecutionProvider", opts), "CPUExecutionProvider"]
        if want == "cuda":
            log.warning("HFML_DEVICE=cuda but ONNX Runtime has no CUDA provider here (%s): using the CPU", ", ".join(avail))
        self.device = "cpu"
        return ["CPUExecutionProvider"]

    def _session(self, path: Path) -> ort.InferenceSession:
        if not path.exists():
            raise FileNotFoundError(f"model not found: {path}")
        so = ort.SessionOptions()
        so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        so.log_severity_level = 3
        if self.cfg.threads > 0:
            so.intra_op_num_threads = self.cfg.threads
        so.inter_op_num_threads = 1
        sess = ort.InferenceSession(str(path), sess_options=so, providers=self.providers)
        if self.device == "cuda" and "CUDAExecutionProvider" not in sess.get_providers():
            log.warning("%s runs on the CPU (the CUDA provider did not load)", path.name)
        return sess

    def _warm(self) -> None:
        """Run every model once, so the first photo is not slow (and a broken model or
        GPU fails at start-up, not on the first request)."""
        t = time.time()
        img = np.full((480, 640, 3), 127, np.uint8)
        cv2.circle(img, (320, 240), 120, (180, 160, 150), -1)
        ok, buf = cv2.imencode(".jpg", img)
        self.analyze(buf.tobytes())
        self.face_emb.run(None, {self.face_emb_input: np.zeros((1, 3, 112, 112), np.float32)})
        self.pet_emb.run(None, {self.pet_emb_input: np.zeros((1, 3, 224, 224), np.float32)})
        if self.siglip is not None:
            self.siglip.embed_images([img[:, :, ::-1]])
        if self.eyes is not None:
            self.eyes.check(np.ascontiguousarray(cv2.resize(img, (320, 320))[:, :, ::-1]))
        log.info("models ready on %s in %.1fs", self.device, time.time() - t)

    # ------------------------------------------------------------------ faces

    def _yunet(self, bgr: np.ndarray, scale: float, thr: float) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        """One YuNet pass on `bgr` resized by `scale`. Returns boxes (x, y, w, h),
        scores and landmarks (5x2), in the coordinates of `bgr`."""
        h0, w0 = bgr.shape[:2]
        if scale < 0.999:
            img = cv2.resize(bgr, (max(1, round(w0 * scale)), max(1, round(h0 * scale))), interpolation=cv2.INTER_AREA)
        else:
            img, scale = bgr, 1.0
        h, w = img.shape[:2]
        ph, pw = (h + 31) // 32 * 32, (w + 31) // 32 * 32
        blob = np.zeros((1, 3, ph, pw), np.float32)
        blob[0, :, :h, :w] = img.transpose(2, 0, 1)
        names = [f"{k}_{s}" for k in ("cls", "obj", "bbox", "kps") for s in (8, 16, 32)]
        outs = dict(zip(names, self.face_det.run(names, {"input": blob})))
        boxes, scores, lms = [], [], []
        for s in (8, 16, 32):
            cols = pw // s
            cls = np.clip(outs[f"cls_{s}"][0, :, 0], 0, 1)
            obj = np.clip(outs[f"obj_{s}"][0, :, 0], 0, 1)
            score = np.sqrt(cls * obj)
            idx = np.nonzero(score >= thr)[0]
            if idx.size == 0:
                continue
            bb = outs[f"bbox_{s}"][0, idx]
            kp = outs[f"kps_{s}"][0, idx].reshape(-1, 5, 2)
            r = (idx // cols).astype(np.float32)
            c = (idx % cols).astype(np.float32)
            cx = (c + bb[:, 0]) * s
            cy = (r + bb[:, 1]) * s
            bw = np.exp(bb[:, 2]) * s
            bh = np.exp(bb[:, 3]) * s
            boxes.append(np.stack([cx - bw / 2, cy - bh / 2, bw, bh], 1))
            scores.append(score[idx])
            lms.append((kp + np.stack([c, r], 1)[:, None, :]) * s)
        if not boxes:
            return np.zeros((0, 4), np.float32), np.zeros(0, np.float32), np.zeros((0, 5, 2), np.float32)
        inv = 1.0 / scale
        return np.concatenate(boxes) * inv, np.concatenate(scores), np.concatenate(lms) * inv

    def detect_faces(self, bgr: np.ndarray, thr: float) -> list[tuple[np.ndarray, float, np.ndarray]]:
        h, w = bgr.shape[:2]
        long_edge = max(h, w)
        boxes, scores, lms = [], [], []
        done = set()
        for target in self.cfg.face_scales:
            scale = min(1.0, target / long_edge)
            key = round(scale, 3)
            if key in done:
                continue
            done.add(key)
            b, s, l = self._yunet(bgr, scale, thr)
            boxes.append(b)
            scores.append(s)
            lms.append(l)
        b = np.concatenate(boxes) if boxes else np.zeros((0, 4))
        s = np.concatenate(scores) if scores else np.zeros(0)
        l = np.concatenate(lms) if lms else np.zeros((0, 5, 2))
        keep = nms(b, s, 0.3)
        return [(b[i], float(s[i]), l[i]) for i in keep]

    def _align(self, bgr: np.ndarray, box: np.ndarray, lm: np.ndarray) -> np.ndarray:
        """The face as ArcFace expects it: 112x112, eyes and mouth on the template."""
        m = umeyama(lm, ARCFACE_DST)
        scale = math.hypot(m[0, 0], m[0, 1])
        if scale < 0.45:
            # A large face: shrink its surroundings with area averaging first, so the
            # affine warp (bilinear) does not alias.
            x, y, bw, bh = box
            cx, cy, side = x + bw / 2, y + bh / 2, max(bw, bh) * 1.6
            x0, y0 = int(max(0, cx - side / 2)), int(max(0, cy - side / 2))
            x1, y1 = int(min(bgr.shape[1], cx + side / 2)), int(min(bgr.shape[0], cy + side / 2))
            crop = bgr[y0:y1, x0:x1]
            f = scale / 0.9
            small = cv2.resize(crop, (max(1, round(crop.shape[1] * f)), max(1, round(crop.shape[0] * f))), interpolation=cv2.INTER_AREA)
            lm2 = (lm - np.array([x0, y0], np.float32)) * np.array(
                [small.shape[1] / max(1, crop.shape[1]), small.shape[0] / max(1, crop.shape[0])], np.float32
            )
            return cv2.warpAffine(small, umeyama(lm2, ARCFACE_DST), (112, 112), flags=cv2.INTER_LINEAR, borderMode=cv2.BORDER_REPLICATE)
        return cv2.warpAffine(bgr, m, (112, 112), flags=cv2.INTER_LINEAR, borderMode=cv2.BORDER_REPLICATE)

    def _face_quality(self, box: np.ndarray, lm: np.ndarray, aligned: np.ndarray, norm: float) -> dict:
        """How usable a face is for recognition, 0..1.

        The length of an ArcFace-family embedding before normalization tracks how
        recognisable the face is: masks, profiles, blur, veils and tiny faces all give
        short ones (measured on wedding photos: about 10 for a masked or turned-away face,
        over 22 for a clear frontal one). Hyperfocal trusts a match between two faces
        less the lower the poorer of them scores.
        """
        size = float(min(box[2], box[3]))
        gray = cv2.cvtColor(aligned, cv2.COLOR_BGR2GRAY)
        sharp = float(cv2.Laplacian(gray[16:104, 16:96], cv2.CV_32F).var())
        eye_mid = (lm[0] + lm[1]) / 2
        eye_dist = float(np.linalg.norm(lm[1] - lm[0])) or 1.0
        mouth_mid = (lm[3] + lm[4]) / 2
        # Nose off the line between the eyes' and the mouth's middles: turned away.
        axis = mouth_mid - eye_mid
        n = float(np.linalg.norm(axis)) or 1.0
        d = lm[2] - eye_mid
        off = float((axis[0] * d[1] - axis[1] * d[0]) / n)
        yaw = min(1.0, abs(off) / eye_dist)
        lo, hi = self.cfg.face_norm_low, self.cfg.face_norm_high
        q = min(1.0, max(0.0, (norm - lo) / max(hi - lo, 1e-6))) * min(1.0, size / 32.0)
        return {"q": round(q, 3), "px": round(size, 1), "sharp": round(sharp, 1), "yaw": round(yaw, 3)}

    def embed_faces(self, faces: list[np.ndarray]) -> tuple[np.ndarray, np.ndarray]:
        """AuraFace embeddings of aligned BGR faces: (normalized, raw norms)."""
        x = np.stack([f[:, :, ::-1] for f in faces]).astype(np.float32)  # RGB
        x = ((x - 127.5) / 127.5).transpose(0, 3, 1, 2)
        out = []
        for i in range(0, len(x), 16):
            out.append(self.face_emb.run(None, {self.face_emb_input: np.ascontiguousarray(x[i : i + 16])})[0])
        e = np.concatenate(out).astype(np.float32)
        norms = np.linalg.norm(e, axis=1)
        return e / np.maximum(norms[:, None], 1e-9), norms

    # ------------------------------------------------------------------ pets

    def detect_animals(self, rgb: np.ndarray, thr: float) -> list[tuple[np.ndarray, float, str]]:
        h, w = rgb.shape[:2]
        n = self.animal_size
        interp = cv2.INTER_AREA if min(h, w) > n else cv2.INTER_LINEAR
        x = cv2.resize(rgb, (n, n), interpolation=interp).astype(np.float32) / 255.0
        x = ((x - IMAGENET_MEAN) / IMAGENET_STD).transpose(2, 0, 1)[None]
        outs = self.animal_det.run(None, {self.animal_input: np.ascontiguousarray(x, dtype=np.float32)})
        boxes, logits = (outs[0], outs[1]) if outs[0].shape[-1] == 4 else (outs[1], outs[0])
        probs = 1.0 / (1.0 + np.exp(-logits[0][:, COCO_ANIMALS]))
        best = probs.argmax(1)
        conf = probs[np.arange(len(best)), best]
        cls = COCO_ANIMALS[best]
        keep = (conf >= thr) & np.isin(cls, list(PETS))
        b = boxes[0][keep]
        if len(b) == 0:
            return []
        x1 = np.clip(b[:, 0] - b[:, 2] / 2, 0, 1)
        y1 = np.clip(b[:, 1] - b[:, 3] / 2, 0, 1)
        x2 = np.clip(b[:, 0] + b[:, 2] / 2, 0, 1)
        y2 = np.clip(b[:, 1] + b[:, 3] / 2, 0, 1)
        xywh = np.stack([x1, y1, x2 - x1, y2 - y1], 1)
        c, k = conf[keep], cls[keep]
        out = []
        # The decoder's queries are deduplicated; the cat-or-dog choice above can still
        # leave a second query on the same animal.
        for i in nms(xywh, c, 0.7):
            if xywh[i, 2] > 0.005 and xywh[i, 3] > 0.005:
                out.append((xywh[i], float(c[i]), PETS[int(k[i])]))
        return out

    def embed_pets(self, crops: list[np.ndarray]) -> np.ndarray:
        x = np.stack(
            [cv2.resize(c, (224, 224), interpolation=cv2.INTER_AREA if min(c.shape[:2]) > 224 else cv2.INTER_CUBIC) for c in crops]
        ).astype(np.float32)
        x = ((x / 255.0 - IMAGENET_MEAN) / IMAGENET_STD).transpose(0, 3, 1, 2)
        out = []
        for i in range(0, len(x), 16):
            out.append(self.pet_emb.run(None, {self.pet_emb_input: np.ascontiguousarray(x[i : i + 16], dtype=np.float32)})[0])
        e = np.concatenate(out).astype(np.float32)
        return e / np.maximum(np.linalg.norm(e, axis=1, keepdims=True), 1e-9)

    # ------------------------------------------------------------------ search

    def embed_images(self, images: list[bytes], lock: threading.Semaphore | None = None) -> list[str | None]:
        """SigLIP 2 embeddings (base64 float16) of encoded images; None for one that
        does not decode."""
        if self.siglip is None:
            raise RuntimeError("search is off (no SigLIP 2 models, or HFML_SEARCH=0)")
        rgbs: list[np.ndarray] = []
        idx: list[int] = []
        for i, data in enumerate(images):
            bgr = cv2.imdecode(np.frombuffer(data, np.uint8), cv2.IMREAD_COLOR | cv2.IMREAD_IGNORE_ORIENTATION)
            if bgr is not None:
                rgbs.append(np.ascontiguousarray(bgr[:, :, ::-1]))
                idx.append(i)
        out: list[str | None] = [None] * len(images)
        if rgbs:
            with lock or _NOLOCK:
                embs = self.siglip.embed_images(rgbs)
            for i, e in zip(idx, embs):
                out[i] = b64_f16(e)
        return out

    def embed_texts(self, texts: list[str], lock: threading.Semaphore | None = None) -> list[str]:
        if self.siglip is None:
            raise RuntimeError("search is off (no SigLIP 2 models, or HFML_SEARCH=0)")
        with lock or _NOLOCK:
            return [b64_f16(e) for e in self.siglip.embed_texts(texts)]

    # ------------------------------------------------------------------ eyes

    def check_eyes(self, images: list[bytes], lock: threading.Semaphore | None = None) -> list[dict | None]:
        """Open or closed eyes of the face in each close-up (see eyes.py); None for one
        that does not decode."""
        if self.eyes is None:
            raise RuntimeError("eye checks are off (no Face Landmarker models, or HFML_EYES=0)")
        out: list[dict | None] = [None] * len(images)
        rgbs: list[tuple[int, np.ndarray]] = []
        for i, data in enumerate(images):
            bgr = cv2.imdecode(np.frombuffer(data, np.uint8), cv2.IMREAD_COLOR | cv2.IMREAD_IGNORE_ORIENTATION)
            if bgr is not None and min(bgr.shape[:2]) >= 32:
                rgbs.append((i, np.ascontiguousarray(bgr[:, :, ::-1])))
        with lock or _NOLOCK:
            for i, rgb in rgbs:
                out[i] = self.eyes.check(rgb)
        return out

    # ------------------------------------------------------------------ entry point

    def analyze(self, data: bytes, faces: bool = True, animals: bool = True, face_thr: float | None = None,
                animal_thr: float | None = None, lock: threading.Semaphore | None = None) -> dict:
        t0 = time.perf_counter()
        bgr = cv2.imdecode(np.frombuffer(data, np.uint8), cv2.IMREAD_COLOR | cv2.IMREAD_IGNORE_ORIENTATION)
        if bgr is None:
            raise ValueError("not a decodable image (send a JPEG or PNG)")
        h, w = bgr.shape[:2]
        rgb = bgr[:, :, ::-1]
        ms: dict[str, float] = {"decode": (time.perf_counter() - t0) * 1000}
        face_out: list[dict] = []
        animal_out: list[dict] = []
        sem = lock or _NOLOCK
        with sem:
            pets: list[tuple[np.ndarray, float, str]] = []
            if animals:
                t = time.perf_counter()
                pets = self.detect_animals(rgb, self.cfg.animal_threshold if animal_thr is None else animal_thr)
                ms["animal_detect"] = (time.perf_counter() - t) * 1000
            if faces:
                t = time.perf_counter()
                found = self.detect_faces(bgr, self.cfg.face_threshold if face_thr is None else face_thr)
                # YuNet takes some dog and cat faces for human ones (with a lower score):
                # a doubtful face that lies mostly inside an animal is the animal's.
                if pets and found:
                    found = [f for f in found if f[1] >= 0.92 or not _inside_animal(f[0], w, h, pets)]
                ms["face_detect"] = (time.perf_counter() - t) * 1000
                if found:
                    t = time.perf_counter()
                    aligned = [self._align(bgr, b, l) for b, _, l in found]
                    emb, norms = self.embed_faces(aligned)
                    ms["face_embed"] = (time.perf_counter() - t) * 1000
                    for (b, s, l), a, e, nrm in zip(found, aligned, emb, norms):
                        x, y, bw, bh = (float(v) for v in b)
                        x0, y0 = max(0.0, x), max(0.0, y)
                        x1, y1 = min(float(w), x + bw), min(float(h), y + bh)
                        if x1 - x0 < 2 or y1 - y0 < 2:
                            continue
                        face_out.append(
                            {
                                "box": [round(x0 / w, 5), round(y0 / h, 5), round((x1 - x0) / w, 5), round((y1 - y0) / h, 5)],
                                "score": round(s, 4),
                                "lm": [round(float(v), 5) for p in l for v in (p[0] / w, p[1] / h)],
                                "norm": round(float(nrm), 3),
                                **self._face_quality(b, l, a, float(nrm)),
                                "emb": b64_f16(e),
                            }
                        )
            if animals:
                crops, idx = [], []
                for i, (b, s, sp) in enumerate(pets):
                    x0, y0 = int(b[0] * w), int(b[1] * h)
                    x1, y1 = int(math.ceil((b[0] + b[2]) * w)), int(math.ceil((b[1] + b[3]) * h))
                    px = min(x1 - x0, y1 - y0)
                    animal_out.append({"box": [round(float(v), 5) for v in b], "score": round(s, 4), "species": sp, "px": px})
                    # Too small to tell one animal from another.
                    if px >= 32:
                        crops.append(np.ascontiguousarray(rgb[y0:y1, x0:x1]))
                        idx.append(i)
                if crops:
                    t = time.perf_counter()
                    for i, e in zip(idx, self.embed_pets(crops)):
                        animal_out[i]["emb"] = b64_f16(e)
                    ms["pet_embed"] = (time.perf_counter() - t) * 1000
        ms["total"] = (time.perf_counter() - t0) * 1000
        return {
            "w": w,
            "h": h,
            "faces": face_out,
            "animals": animal_out,
            "ms": {k: round(v, 1) for k, v in ms.items()},
        }


def _inside_animal(box: np.ndarray, w: int, h: int, pets: list[tuple[np.ndarray, float, str]]) -> bool:
    """Whether at least 60% of a face box (pixels) lies inside one of the animal boxes
    (normalized)."""
    x, y, bw, bh = (float(v) for v in box)
    area = max(bw * bh, 1e-9)
    for b, _, _ in pets:
        ax0, ay0, ax1, ay1 = b[0] * w, b[1] * h, (b[0] + b[2]) * w, (b[1] + b[3]) * h
        iw = max(0.0, min(x + bw, ax1) - max(x, ax0))
        ih = max(0.0, min(y + bh, ay1) - max(y, ay0))
        if iw * ih / area >= 0.6:
            return True
    return False


class _NoLock:
    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


_NOLOCK = _NoLock()
