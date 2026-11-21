#!/bin/sh
# Converts MediaPipe's Face Landmarker models (float16 TFLite) to the ONNX files kept here.
# Needs Python 3.13 and about 2 GB for a throwaway virtual environment.
#   sh ml/models/convert.sh [out-dir]
set -eu
out="${1:-$(dirname "$0")}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl -fsSL -o "$work/face_landmarker.task" \
  https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task
echo "64184e229b263107bc2b804c6625db1341ff2bb731874b0bcc2fe6544e0bc9ff  $work/face_landmarker.task" | sha256sum -c -
(cd "$work" && unzip -q face_landmarker.task)

python3 -m venv "$work/venv"
"$work/venv/bin/pip" install -q tf2onnx==1.17.0 tensorflow-cpu==2.21.0 onnx==1.23.2
"$work/venv/bin/python" -m tf2onnx.convert --opset 15 --tflite "$work/face_landmarks_detector.tflite" --output "$out/face-landmarks.onnx"
"$work/venv/bin/python" -m tf2onnx.convert --opset 15 --tflite "$work/face_blendshapes.tflite" --output "$out/face-blendshapes.onnx"
sha256sum "$out/face-landmarks.onnx" "$out/face-blendshapes.onnx"
echo "Update their sha256s (and sizes) in server/src/ml/manifest.json."
