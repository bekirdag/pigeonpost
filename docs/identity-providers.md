# Pigeonpost — Identity provider setup

Status: operator guide. Applies to the registry only.
Opened: 2026-08-08

Handle registration is gated on proving control of an identity that already exists and is already
spam-defended (`architecture.md`). This is how to register the applications that make that work.

Nothing here touches key addresses — those need no provider, no registration, and no human.

## What each provider is, and what it gives you

The providers are **not** uniform, and treating them as one thing is how this gets built wrong.

| | **GitHub** | **Google** |
| --- | --- | --- |
| App type | **OAuth App** — *not* a GitHub App | OAuth 2.0 Client ID (Web application) |
| Protocol | OAuth 2.0 authorization code | Genuine OIDC (ID token) |
| Why | GitHub issues **no user-facing OIDC**; its OIDC issuer serves Actions workflows only | — |
| Registry needs | client id **and secret** | client id only |
| Secret used for | Server-side code exchange | Nothing — the ID token is verified against Google's public JWKS |
| Handle you get | `/github/<login>` — e.g. `/github/superaidev` | `/google/<sub>` — e.g. `/google/104729183746501928374` |

That last row matters more than it looks. See §5.

## 1. GitHub OAuth App

1. **github.com → Settings → Developer settings → OAuth Apps → New OAuth App**
   (An *OAuth App*. A *GitHub App* is a different product and will not work with this code.)
2. Fill in:
   - **Application name** — `Pigeonpost`
   - **Homepage URL** — `https://pigeonpost.dev`
   - **Authorization callback URL** — `http://127.0.0.1:8765/callback`
     The CLI binds this exact loopback listener before it requests a short-lived challenge.
3. **Register application**, then **Generate a new client secret**.
4. Keep the **Client ID** and **Client secret**. The secret is shown once.

The registry exchanges the code at `https://github.com/login/oauth/access_token` and reads the
account login from `https://api.github.com/user`. Every challenge requires PKCE S256; the registry
forwards the matching `code_verifier` during the code exchange.

**Scopes:** none needed. The default gives the public profile, which is all we read.

## 2. Google OAuth client

1. **console.cloud.google.com** → create or pick a project.
2. **APIs & Services → OAuth consent screen**
   - User type **External**, unless everyone claiming a handle is in your Workspace
   - App name, support contact, developer contact
   - **Scopes: `openid profile`.** This is the minimum eligible pair Pigeonpost uses for the
     ID-token flow. It discards every optional profile claim and binds only the stable `sub`
   - Publish it, or claims will be limited to test users you list by hand
3. **APIs & Services → Credentials → Create credentials → OAuth client ID**
   - Application type **Web application**
   - **Authorized redirect URIs** — `http://127.0.0.1:8765/callback`
4. Keep the **Client ID**. The client secret exists but the registry never uses it: it verifies the
   ID token's RS256 signature against `https://www.googleapis.com/oauth2/v3/certs`, with the issuer
   pinned to `accounts.google.com` and the audience pinned to this client id.

## 3. Configure the registry

Provider credentials activate a fail-closed production boundary. Before setting them, provision
`registry.toml`, purpose-separated network/identity trace signing files, a fresh witnessed
compliance-key cache, at least one pinned directory publisher public key, and the external custody
keys described in
[`runtime-configuration.md`](runtime-configuration.md). The registry refuses provider mode when
any part is absent or stale; a credential-free registry can still serve authenticated reads.

Provision the GitHub secret as a single-line, no-newline file before restarting. For the supplied
Compose topology the container runs as uid/gid 10001, so the bind-mounted host file must have that
identity and owner-only mode:

```bash
sudo install -d -o 10001 -g 10001 -m 0700 /opt/pigeonpost/secrets
sudo install -o 10001 -g 10001 -m 0400 /dev/null \
  /opt/pigeonpost/secrets/github-client-secret
read -r -s -p 'GitHub client secret: ' github_secret; printf '\n'
printf '%s' "$github_secret" \
  | sudo tee /opt/pigeonpost/secrets/github-client-secret >/dev/null
unset github_secret

export PIGEONPOST_GITHUB_CLIENT_ID=Iv1.xxxxxxxxxxxx
export PIGEONPOST_GITHUB_CLIENT_SECRET_FILE=/opt/pigeonpost/secrets/github-client-secret
export PIGEONPOST_GOOGLE_CLIENT_ID=xxxxxxxxxxxx.apps.googleusercontent.com
```

For a native service, use the registry service account instead of uid/gid 10001. The runtime rejects
links, multiple hard links, non-regular or empty files, files larger than 4 KiB, whitespace/control
characters, the wrong owner, and modes other than `0400`/`0600`. Each provider is registered only
when its complete configuration is present, and absence is not an error—a registry with no
credentials still serves `resolve` and the log dump. That is deliberate: the read path stays up
without secrets.

Confirm from the startup banner:

```
registry listening on 0.0.0.0:7718
  providers   github, google
```

`providers none` means the public IDs or mounted file did not resolve. The Compose templates put
only public IDs and the fixed in-container `PIGEONPOST_GITHUB_CLIENT_SECRET_FILE` path in
`environment:`; the secret value never enters the container environment. The host file path is
consumed by Compose only to create the read-only bind mount.

Direct `PIGEONPOST_GITHUB_CLIENT_SECRET` is a loopback-development compatibility path only. It also
requires `PIGEONPOST_ALLOW_INSECURE_PROVIDER_SECRET_ENV=1`; production preflight rejects both the
flag and any direct-secret variable.

Production Pigeonpost binaries do not contain the mock identity provider. It is available only to
source tests through the explicit `test-utilities` feature, and production preflight rejects both
the retired and source-test mock flags.

## 4. Claim through the local callback

The registry exposes a challenge endpoint and accepts only a proof bound to a live, single-use
challenge. Issuance itself is signed by the agent key, and the stored challenge is bound to the
exact canonical handle and public key; a captured provider response cannot race a different key.
The CLI owns the browser callback, while the registry never receives a browser redirect or grows
browser-session state. GitHub PKCE S256 is mandatory.

```bash
pigeonpost handle claim /github/superaidev \
  --registry https://registry.pigeonpost.dev
```

The command first requires configured witnessed registry trust, binds exactly
`127.0.0.1:8765`, requests an authenticated five-minute challenge, validates the provider metadata,
prints the authorization URL, and opens the system browser. GitHub returns a code through query
parameters; Google returns an ID token in a URI fragment that a no-store loopback page relays to the
same one-shot listener. The listener requires the exact loopback `Host`, requires the exact
same-origin `Origin` on Google's relay POST, and serves only hash-authorized scripts. PKCE, state,
nonce, response size, fragment size, connection count, and time are all bounded.

For a remote or headless terminal, use manual mode:

```bash
pigeonpost handle claim /github/superaidev \
  --registry https://registry.pigeonpost.dev \
  --no-browser
```

Manual mode opens no loopback port. Open the printed authorization URL on any machine; after the
provider redirects to the fixed loopback URL, copy the **full address-bar URL** and paste it at the
non-echoing terminal prompt. The PKCE verifier remains only in the original Pigeonpost process.
The browser may show a connection error because no listener exists; the callback URL in its address
bar is still the value to paste.

Pigeonpost intentionally has no CLI flags for provider codes, PKCE verifiers, states, nonces, or ID
tokens. Putting those values in process arguments exposes them through shell history and process
inspection. Programmatic integrations should use the MCP tool's challenge-bound begin/complete
operations instead.

Do **not** add a callback route to the registry to solve this. The redirect would have to carry the
agent's public key and a signature through a browser round trip, and the registry would grow a
session concept it does not otherwise need.

## 5. Google handles are opaque numbers — decide before you enable it

Since the fix that stopped a mutable contact-derived identifier reaching the permanent public log
(`law.md`, Phase 0), the Google namespace uses the OIDC `sub` claim. That is correct for privacy and
correct on its own terms — contact addresses get reassigned, `sub` does not — but it means a Google handle
looks like:

```
/google/104729183746501928374
```

The entire point of the handle tier is a **human-readable** name (`architecture.md`). A 21-digit
number is not one, and it is strictly worse than the key address it would alias, which is at least
shorter.

So the honest options are:

- **Enable GitHub only.** `/github/<login>` is exactly what the tier is for. This is the recommendation.
- **Enable Google anyway** for people who have no GitHub account, accepting that those handles are
  machine identifiers with extra steps.
- **Drop the Google namespace** until a provider is added whose subject is both stable and
  human-meaningful — GitLab usernames and npm package owners both qualify, and
  `architecture.md` already names them.

Whatever you choose, do not "fix" it by going back to a contact-address local part. That was the Phase 0
bug, the log is public and append-only, and a personal identifier written there cannot be retracted.

## 6. Verifying it works

```bash
# Resolve is open and needs no credentials at all.
curl https://registry.pigeonpost.dev/v1/resolve/github/superaidev

# The whole log, verifiable by anyone.
curl https://registry.pigeonpost.dev/v1/log/dump

# Claim through the bounded local browser callback.
pigeonpost handle claim /github/<your-login> \
  --registry https://registry.pigeonpost.dev

# Rebind the same handle to this agent's current key, including from a fresh home after key loss.
pigeonpost handle rotate /github/<your-login> \
  --registry https://registry.pigeonpost.dev

# First import the independently obtained trust bundle, then resolve through that durable root.
pigeonpost registry-trust import --file registry-trust.json
pigeonpost handle resolve /github/<your-login> --registry https://registry.pigeonpost.dev
```

A claim must satisfy three things: the identity proof authenticates a subject, that subject matches
the handle being claimed, and both challenge issuance and registration are authenticated by the key
being bound. The last property is what stops anyone binding a handle to someone else's public key.
The public-name match is a first-claim rule. If a provider later renames the same account, a fresh
proof for the same stable provider subject may rotate the key of the exact original handle; the
registry keeps that original spelling. A rename never moves or aliases the binding, never permits
the stable subject to claim a second spelling, and a later account that inherits the old public name
cannot rotate the original handle because its stable subject differs.
The rotate command uses a fresh challenge and provider proof, then waits for the exact
`handle_rotate` receipt leaf under a fresh witnessed checkpoint. This is the supported total-key-loss
recovery path: it restores future delivery to the handle, not the lost key address, local state, or
old ciphertext.
Resolve requires an existing agent state with strict-majority witnessed-registry trust (`2k > N`).
It verifies a fresh witness quorum, exact leaf inclusion, and append-only consistency from the
configured minimum or last accepted checkpoint. The strict majority guarantees set intersection
for one roster, not witness honesty; the full `f < 2k - N` fault assumption and cross-roster rules
are specified in [`runtime-configuration.md`](runtime-configuration.md). Inclusion alone is not
freshness: the client also audits every leaf
through that exact witnessed head, applies the strict claim/rotation and stable-provider-subject
state machine to one global normalized handle projection, and requires the server's candidate to
equal the latest derived binding. A fresh client replays immutable exact NDJSON ranges in bounded
segments; safe delivery failures fall back to bounded JSON pages, while malformed/state/root
failures remain terminal. All later handle lookups process only appended leaves. Replay is staged
outside the agent database write lock, then the
projection delta, compact frontier, requested cache row, and sole checkpoint pin commit atomically.
The CLI reports only the registry status on refusal and never reflects an untrusted registry body.

### Pre-1.0 `/gh` migration

The short `/gh/<login>` spelling used by pre-1.0 development builds is deliberately not an alias for
`/github/<login>`. Registries reject new `/gh` challenges, claims, rotations, and resolutions. An
authenticated v0.1 database migration preserves the original `/gh` leaves byte-for-byte so old
checkpoints remain verifiable and the source rows remain auditable; it does not publish those rows
through the resolver. Re-prove the provider identity and claim `/github/<login>` before advertising
the canonical handle.
