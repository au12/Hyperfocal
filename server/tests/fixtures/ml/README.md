`face-open.jpg` and `face-closed.jpg`: AI-generated portraits (no real person) from the
[Open and Closed Eyes Dataset](https://huggingface.co/datasets/MichalMlodawski/closed-open-eyes)
by Michał Młodawski, licensed under the Open Data Commons Attribution License (ODC-By
1.0); images 119921 (eyes open) and 63909 (eyes closed), each face cropped as Hyperfocal
sends it to the AI engine's `/v1/eyes`. Used by the engine's tests (`src/ml/engine/tests.rs`)
and by the image build's self-test of the eye models.
