"""Private, offline Phonon-2 worker. The only content channel is inherited IPC."""
import importlib.metadata
import json
import os
from pathlib import Path
import struct
import sys

# Even native libraries cannot put content into stdout/stderr diagnostics.
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

    # Direct MLX loader; no backend auto-selection, model hub, or lazy fetch.
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
        # Keep existing Phonon cleanup behaviour: a flat transcript with no
        # synthetic pause breaks. Cleanup remains a separate user control.
        send({"text": result.text, "segments": []})
        del result, audio


try:
    run()
except BaseException:
    # A dependency exception may quote a partial transcript. Never forward it.
    send({"error": "Phonon-2 MLX could not run. Reinstall it from Settings."})
    sys.exit(1)
