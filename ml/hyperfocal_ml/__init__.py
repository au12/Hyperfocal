"""Hyperfocal ML: face and pet detection and recognition for Hyperfocal.

A stateless inference service. Hyperfocal sends it an upright JPEG; it answers with
the faces, dogs and cats it found, each with an embedding Hyperfocal clusters and
matches. It keeps nothing and never sees the photo library.
"""

__version__ = "1.0.0"
