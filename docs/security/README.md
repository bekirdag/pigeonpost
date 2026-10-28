# Security review material

| Document | For |
|---|---|
| [`groupcrypto-review-packet.md`](groupcrypto-review-packet.md) | External review of `crates/pigeonpost-groupcrypto`: scope, threat model, reviewer questions, build steps and known-answer vectors (`crates/pigeonpost-groupcrypto/tests/vectors/`). |
| [`agent-ssh-pentest-scope.md`](agent-ssh-pentest-scope.md) | Penetration test of agent SSH (postbox shell API, tunnel broker, agentd SSH server, CLI client): targets, environments, rules of engagement, test cases. |

The related advisory notes are `docs/security-advisory-2026-10-24.md` and the protocol
documents `docs/group-e2ee.md` and `docs/agent-ssh.md`.
