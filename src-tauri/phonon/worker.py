"""Private, offline Phonon-2 worker. The only content channel is inherited IPC."""
import importlib.metadata
import json
import os
from pathlib import Path
import struct
import sys

protocol = os.fdopen(os.dup(sys.stdout.fileno()), "wb", buffering=0)
null = os.open(os.devnull, os.O_WRONLY)
os.dup2(null, 1)
os.dup2(null, 2)
os.close(null)


def send(value):
    protocol.write(json.dumps(value, ensure_ascii=False).encode("utf-8") + b"\n")


def read_exact(count):
    chunks = bytearray()
    while len(chunks) < count:
        part = sys.stdin.buffer.read(count - len(chunks))
        if not part:
            raise EOFError
        chunks.extend(part)
    return chunks


def run():
    root = Path(sys.argv[1])
    sys.path.insert(0, str(root / "site"))
    if importlib.metadata.version("fermion-research") != "0.2.7":
        raise RuntimeError
    import numpy as np
    from fermion._speech.engine_phonon2 import load

    model = load(root / "model", profile="five-value",
                 backend="phonon2-five-value", quiet=True)
    if model.decode.get("levers_error"):
        raise RuntimeError
    model.transcribe_array(np.zeros(16000, dtype=np.float32))
    send({"ready": True})
    while True:
        try:
            count = struct.unpack("<I", read_exact(4))[0]
        except EOFError:
            return
        if count == 0 or count > 16000 * 60 * 30:
            raise ValueError
        audio = np.frombuffer(read_exact(count * 4), dtype="<f4")
        if not np.isfinite(audio).all():
            raise ValueError
        result = model.transcribe_array_detailed(audio)
        send({"text": result.text, "segments": []})
        del result, audio


try:
    run()
except BaseException:
    send({"error": "Phonon-2 MLX could not run. Reinstall it from Settings."})
    sys.exit(1)
