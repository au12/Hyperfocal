# Models kept in this repository

`fetch_models.py` downloads the other models of the image from their publishers. These two
have no ONNX release, so they are converted once and kept here; the image build copies them
after checking their sha256 (in `../models.json`).

| File | What | Source |
| --- | --- | --- |
| `face-landmarks.onnx` | MediaPipe Face Mesh V2: 478 face landmarks from a 256 × 256 crop | `face_landmarks_detector.tflite` |
| `face-blendshapes.onnx` | MediaPipe Blendshape V2: 52 expression coefficients from 146 of the landmarks | `face_blendshapes.tflite` |

Both are the float16 TFLite models of Google's Face Landmarker bundle, version 1:

    https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task
    sha256 64184e229b263107bc2b804c6625db1341ff2bb731874b0bcc2fe6544e0bc9ff

Licence: Apache-2.0 (models and code), © Google LLC; the licence text is in
`LICENSE-mediapipe`. Model cards:
[Face Mesh V2](https://storage.googleapis.com/mediapipe-assets/Model%20Card%20MediaPipe%20Face%20Mesh%20V2.pdf),
[Blendshape V2](https://storage.googleapis.com/mediapipe-assets/Model%20Card%20Blendshape%20V2.pdf).

The only change is the format: the weights and the graph are the same, converted with
tf2onnx 1.17.0 (TensorFlow 2.21.0, onnx 1.23.2, opset 15) by `convert.sh`. tf2onnx does not
write byte-identical files from run to run, so a new conversion has other sha256s; its
outputs are numerically identical (checked on random inputs: largest difference 0).

`hyperfocal_ml/eyes.py` uses them as MediaPipe's own graph does: the face 1.5 times enlarged
and turned upright for the mesh model, the 146 landmarks MediaPipe lists in
`face_blendshapes_graph.cc` for the blendshape model (which centres and scales them itself).
On 300 portraits its eyeBlink scores are within 0.05 of the MediaPipe Python package's on
average.
