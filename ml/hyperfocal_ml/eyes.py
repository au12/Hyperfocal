"""Open or closed eyes: MediaPipe Face Landmarker (Google, Apache-2.0) on face close-ups.

Hyperfocal sends a square close-up of each face it already knows: the face's box
centred, the close-up `CROP_SIDE` times the box's long side. The face mesh model (478
landmarks) runs on a 256 x 256 crop of it, MediaPipe's way: the face 1.5 times
enlarged, turned upright on the line between the eyes. The landmarks of a face more
than about 10 degrees off upright are taken again from the crop turned with them (the
mesh model is trained on upright faces). MediaPipe's blendshape model then gives the
52 ARKit-style expression coefficients from 146 of the landmarks, among them
eyeBlinkLeft and eyeBlinkRight (0 open, 1 closed; Left is the subject's left eye, on
the right of the picture).

Measured on 300 portraits (200 with closed eyes): open eyes score at most 0.24,
closed ones at least 0.43 for 98% of them. A face turned so far that one eye is half
hidden is judged by the eye that shows.

The two models are the float16 TFLite files of the face_landmarker.task bundle (v1),
converted to ONNX: see ml/models/README.md.
"""

from __future__ import annotations

import math
from pathlib import Path

import cv2
import numpy as np

LANDMARKS = "face-landmarks.onnx"
BLENDSHAPES = "face-blendshapes.onnx"

# The close-up Hyperfocal sends: the face box's long side times this, centred on it.
CROP_SIDE = 2.2
# MediaPipe's region of interest: the face 1.5 times enlarged.
ROI_SCALE = 1.5
SIZE = 256

# The 146 landmarks the blendshape model reads (MediaPipe's kLandmarksSubsetIdxs, in
# face_blendshapes_graph.cc).
SUBSET = np.array([
    0, 1, 4, 5, 6, 7, 8, 10, 13, 14, 17, 21, 33, 37, 39, 40, 46, 52, 53, 54, 55, 58, 61, 63, 65, 66, 67, 70,
    78, 80, 81, 82, 84, 87, 88, 91, 93, 95, 103, 105, 107, 109, 127, 132, 133, 136, 144, 145, 146, 148, 149,
    150, 152, 153, 154, 155, 157, 158, 159, 160, 161, 162, 163, 168, 172, 173, 176, 178, 181, 185, 191, 195,
    197, 234, 246, 249, 251, 263, 267, 269, 270, 276, 282, 283, 284, 285, 288, 291, 293, 295, 296, 297, 300,
    308, 310, 311, 312, 314, 317, 318, 321, 323, 324, 332, 334, 336, 338, 356, 361, 362, 365, 373, 374, 375,
    377, 378, 379, 380, 381, 382, 384, 385, 386, 387, 388, 389, 390, 397, 398, 400, 402, 405, 409, 415, 454,
    466, 468, 469, 470, 471, 472, 473, 474, 475, 476, 477,
], np.int64)
EYE_BLINK_LEFT, EYE_BLINK_RIGHT = 9, 10
# Eye corners on the mesh: the eye on the left of the picture (the subject's right).
PIC_LEFT_EYE = (33, 133)
PIC_RIGHT_EYE = (362, 263)
# Turned this far from upright (degrees): the landmarks are taken again, turned.
REFINE_DEG = 10.0


def available(models_dir: Path) -> bool:
    return all((models_dir / f).exists() for f in (LANDMARKS, BLENDSHAPES))


def roi_matrix(cx: float, cy: float, side: float, angle: float) -> np.ndarray:
    """The 2x3 affine that maps a square of `side` centred on (cx, cy), turned by
    `angle` (radians, the eyes' line against the x axis), onto the model's input."""
    c, s = math.cos(angle), math.sin(angle)
    k = SIZE / side
    m = np.array([[k * c, k * s, 0.0], [-k * s, k * c, 0.0]], np.float64)
    m[:, 2] = np.array([SIZE / 2, SIZE / 2]) - m[:, :2] @ np.array([cx, cy])
    return m


def roi_of(points: np.ndarray) -> tuple[float, float, float, float]:
    """MediaPipe's region for a face from its landmarks: turned on the line between
    the outer eye corners (33 to 263), square around the turned bounding box, 1.5
    times enlarged."""
    a = points[263] - points[33]
    angle = math.atan2(float(a[1]), float(a[0]))
    c, s = math.cos(angle), math.sin(angle)
    turned = points @ np.array([[c, -s], [s, c]])  # into the face's frame
    lo, hi = turned.min(0), turned.max(0)
    mid = (lo + hi) / 2
    centre = mid @ np.array([[c, s], [-s, c]])  # back
    return float(centre[0]), float(centre[1]), float(max(hi - lo)) * ROI_SCALE, angle


class Eyes:
    def __init__(self, models_dir: Path, session) -> None:
        """`session(path)` makes an ONNX Runtime session the way the pipeline does."""
        self.mesh = session(models_dir / LANDMARKS)
        self.mesh_input = self.mesh.get_inputs()[0].name
        self.shapes = session(models_dir / BLENDSHAPES)
        self.shapes_input = self.shapes.get_inputs()[0].name

    def _mesh(self, rgb: np.ndarray, roi: tuple[float, float, float, float]) -> tuple[np.ndarray, float]:
        """The 478 landmarks (picture coordinates) and the face presence probability."""
        m = roi_matrix(*roi)
        crop = cv2.warpAffine(rgb, m, (SIZE, SIZE), flags=cv2.INTER_LINEAR, borderMode=cv2.BORDER_CONSTANT, borderValue=0)
        x = (crop.astype(np.float32) * (1.0 / 255.0))[None]
        out = self.mesh.run(None, {self.mesh_input: x})
        pts = out[0].reshape(-1, 3)[:, :2].astype(np.float64)
        logit = float(np.asarray(out[1]).reshape(-1)[0])
        inv = cv2.invertAffineTransform(m)
        return pts @ inv[:, :2].T + inv[:, 2], 1.0 / (1.0 + math.exp(-max(-30.0, min(30.0, logit))))

    def check(self, rgb: np.ndarray) -> dict:
        """Eyes of the face in the middle of a close-up (RGB, upright)."""
        h, w = rgb.shape[:2]
        side = min(w, h) / CROP_SIDE * ROI_SCALE
        pts, p = self._mesh(rgb, (w / 2, h / 2, side, 0.0))
        roi = roi_of(pts)
        if abs(math.degrees(roi[3])) > REFINE_DEG or p < 0.5:
            pts2, p2 = self._mesh(rgb, roi)
            if p2 >= p or p < 0.5:
                pts, p = pts2, p2
        shapes = self.shapes.run(None, {self.shapes_input: pts[SUBSET].astype(np.float32)[None]})[0].reshape(-1)
        # Picture left and right (the model's Left is the subject's left eye).
        left, right = float(shapes[EYE_BLINK_RIGHT]), float(shapes[EYE_BLINK_LEFT])
        wl = float(np.linalg.norm(pts[PIC_LEFT_EYE[0]] - pts[PIC_LEFT_EYE[1]]))
        wr = float(np.linalg.norm(pts[PIC_RIGHT_EYE[0]] - pts[PIC_RIGHT_EYE[1]]))
        # A face turned so far that one eye is foreshortened to half the other: that
        # eye's estimate is a guess, so the other one decides.
        if wl < 0.5 * wr:
            closed = right
        elif wr < 0.5 * wl:
            closed = left
        else:
            closed = (left + right) / 2
        eyes = np.array([pts[[33, 133, 159, 145]].mean(0), pts[[362, 263, 386, 374]].mean(0)])
        return {
            "closed": round(closed, 3),
            "blink": [round(left, 3), round(right, 3)],
            "p": round(p, 3),
            "angle": round(math.degrees(roi[3]), 1),
            # The eyes' centres in the close-up (0..1): where to look.
            "eyes": [[round(float(e[0]) / w, 4), round(float(e[1]) / h, 4)] for e in eyes],
        }
