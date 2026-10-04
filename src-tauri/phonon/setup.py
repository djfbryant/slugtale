"""Runs only from the explicit Install action, before any audio exists."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

root = Path(sys.argv[1])
subprocess.run([
    sys.executable, "-I", "-m", "pip", "--isolated", "install",
    "--index-url", "https://pypi.org/simple", "--require-hashes",
    "--only-binary=:all:", "--no-deps", "--no-compile", "--no-cache-dir",
    "--disable-pip-version-check", "--target", str(root / "site"),
    "-r", str(root / "requirements.lock"),
], check=True, timeout=900)
sys.path.insert(0, str(root / "site"))
from fermion._speech.fetch import _unpack

# The archive was size/SHA-256 checked by Rust before this trusted unpacker.
_unpack(root / "phonon-2.bps.tar.zst", root / "model")
manifest = json.loads((root / "packed_manifest.json").read_text())
container = root / "model" / "model.fermion"
with container.open("rb") as source:
    digest = hashlib.file_digest(source, "sha256").hexdigest()
if digest != manifest["container_sha256"] or container.stat().st_size != manifest["container_bytes"]:
    raise RuntimeError("Phonon-2 container check failed")
for name in ["NOTICE", "LICENSE-CODE-Apache-2.0.txt", "LICENSE-WEIGHTS-CC-BY-4.0.txt"]:
    shutil.copyfile(root / name, root / "model" / name)
