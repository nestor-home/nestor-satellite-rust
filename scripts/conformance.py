"""Run the Nestor server's black-box conformance suite against the `nestor-satellite` binary.

    ../nestor/.venv/bin/python scripts/conformance.py target/debug/nestor-satellite

The suite runs a fake core (the real pairing service and satellite gateway) and launches the binary
against it once per scenario, as a robot-less speaker (audio.capture + audio.playback +
device.identify, no sound card): `{url}` is the core's URL, `{state}` a fresh folder for the token,
and the enrollment code comes in NESTOR_PAIRING_URI. It checks pairing with a device code and with
an enrollment code, refused codes, rate limits, identify, hello, acks, audio both ways and
reconnection. Exit status 0 when every check passes.
"""

import asyncio
import sys

from nestor.adapters.channels.voice.satellites.conformance import check_satellite


async def main(binary: str) -> int:
    command = [binary, "run", "--core", "{url}", "--room", "salon", "--id", "enceinte-salon",
               "--model", "conformance", "--token-file", "{state}/token.json", "--null-audio", "--verbose"]  # fmt: skip
    findings = await check_satellite(command, deadline_s=15)
    for finding in findings.items:
        print(f"{'PASS' if finding.passed else 'FAIL'}  {finding.check}" + (f" ({finding.detail})" if finding.detail and not finding.passed else ""))
    print("conformant" if findings.passed else f"{len(findings.failures())} check(s) failed")
    return 0 if findings.passed else 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main(sys.argv[1])))
