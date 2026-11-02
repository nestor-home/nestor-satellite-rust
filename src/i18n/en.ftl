# What a satellite says to people, in English. Short sentences, no em-dash.

pairing-code-shown = To pair this device, type this code in Nestor:
# Said aloud, digits one by one. The bundled voice plays a recorded clip for each piece of text between
# the digits (voice/, recorded by nestor-satellite-python's scripts/voice.py): record them again after a change.
pairing-code-spoken = Pairing code: { $digits }. I repeat: { $digits }.
pairing-headless = This device has no screen and no voice: to pair it, create an enrollment code in Nestor and set NESTOR_PAIRING_URI.
pairing-paired = Paired with Nestor.
pairing-expired = The pairing code expired. Here is a new one.
pairing-refused = { $reason ->
        [expired] This enrollment code has expired.
        [used] This enrollment code was already used.
        [cancelled] This enrollment code was cancelled.
       *[unknown] This enrollment code is not valid.
    } Use the device code instead.
pairing-already-refused = This enrollment code was already refused.
pairing-rate-limited = Too many wrong codes. Trying again in { $seconds } s.
pairing-busy = Too many devices are waiting to pair. Trying again in { $seconds } s.
identify-requested = Nestor asks this device to show itself.

error-protocol-unsupported = Nestor does not speak this device's protocol: update the device or Nestor.
error-pairing-refused = Nestor refused the pairing request: update the device.
error-not-pairing = This device is not waiting for a code.
error-method-not-declared = This device does not take enrollment codes that way.
error-enrollment-code = An enrollment code is 8 letters and digits, like K7QF-2M9X.
error-other-core = This pairing URI is for another Nestor.
error-token-refused = Nestor no longer knows this device: pair it again.
error-replaced = Another connection of this device took over: this one stops.
error-unreachable = Nestor cannot be reached.
