"""Download the models listed in models.json into a folder and check their sha256.

    python fetch_models.py /models

Files already present with the right hash are kept. Exits non-zero on any mismatch.
"""

import hashlib
import json
import sys
import urllib.request
from pathlib import Path


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(4 << 20):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    dest = Path(sys.argv[1] if len(sys.argv) > 1 else "/models")
    dest.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((Path(__file__).parent / "models.json").read_text())
    for m in manifest["models"]:
        out = dest / m["file"]
        if out.exists() and sha256(out) == m["sha256"]:
            print(f"ok      {m['file']}")
            continue
        print(f"fetch   {m['file']} <- {m['url']}", flush=True)
        tmp = out.with_suffix(".part")
        req = urllib.request.Request(m["url"], headers={"User-Agent": "hyperfocal-ml-build"})
        with urllib.request.urlopen(req, timeout=600) as r, open(tmp, "wb") as f:
            while chunk := r.read(4 << 20):
                f.write(chunk)
        got = sha256(tmp)
        if got != m["sha256"]:
            tmp.unlink()
            print(f"error   {m['file']}: sha256 {got}, expected {m['sha256']}", file=sys.stderr)
            return 1
        tmp.rename(out)
    (dest / "MODELS.json").write_text(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
