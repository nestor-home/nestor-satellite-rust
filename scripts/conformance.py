"""Run the Nestor server's black-box conformance suite against the `nestor-satellite` binary.

    ../nestor/.venv/bin/python scripts/conformance.py target/debug/nestor-satellite

The suite runs a fake core (the real pairing service and satellite gateway), launches the binary
against it as a robot-less speaker (audio.capture + audio.playback, no sound card), approves the
pairing code and checks pairing, hello, acks, audio both ways and reconnection. Exit status 0 when
every check passes.
"""

import asyncio
import sys
import tempfile
from pathlib import Path

from nestor.adapters.channels.voice.satellites.conformance import check_satellite


async def main(binary: str) -> int:
    with tempfile.TemporaryDirectory() as folder:
        command = [binary, "run", "--core", "{url}", "--room", "salon", "--id", "enceinte-salon",
                   "--model", "conformance", "--token-file", str(Path(folder) / "token.json"),
                   "--null-audio", "--verbose"]  # fmt: skip
        findings = await check_satellite(command, deadline_s=15)
    for finding in findings.items:
        print(f"{'PASS' if finding.passed else 'FAIL'}  {finding.check}" + (f" ({finding.detail})" if finding.detail and not finding.passed else ""))
    print("conformant" if findings.passed else f"{len(findings.failures())} check(s) failed")
    return 0 if findings.passed else 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main(sys.argv[1])))
