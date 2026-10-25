# Privacy fixture port allocation

Linux integration #105 failed first when Loft's listener address was already in use, then on
rerun when a new registry listener reused an earlier directory client's address. The latter
produced a false privacy disclosure from a legitimate server-startup line.

- Keep fixture server ports outside the running kernel's automatic client-port range on Linux
  and macOS. Reserve actual loopback listeners, retain existing startup sequencing and avoid
  reissuing a server port within the test process.
- Exercise the released-listener startup gap with real bound clients, and ensure a later
  server cannot reuse a previously bound client address. Preserve every privacy/trace assertion.
- Run the complete privacy gate with `test-utilities`, including its negative scanner cases,
  format and lint checks, then qualify both CI platforms. No production source or deployment.

Impact: the one integration-test binary. Docdex symbols and scoped file reads identify the
reservation/HTTP helpers; the impact graph reports no external edges. Caller inspection confirms
the new allocation applies to the fixture's registry, Loft and directory processes.
