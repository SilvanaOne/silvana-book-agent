# Agent control: operator guide

An agent can be governed by **controllers**: passkeys created on the Agent page, or Ed25519 keys
you keep yourself. A controller holds one or more capabilities:

| Capability | What it allows |
| --- | --- |
| `policy` | set the policy the agent follows |
| `direct` | direct one transaction |
| `read` | read the agent's transactions |

Every change to the controllers is an entry in a **control log** that the agent signs with its own
key and chains by hash. The agent keeps the log in its control directory, and the rpc keeps a copy.
Linking is optional: an agent that was never linked behaves as before.

## Before you start

- `CANTON_CHAIN` must be `devnet` or `mainnet`. Control commands refuse any other value.
- The agent's `.env` must be known (the one in the working directory, or `--env-file`). The agent
  records the head of its control log there as `AGENT_CONTROL_HEAD=<network>:<seq>:<hash>`.
- The control directory is `$SILVANA_CONTROL_DIR`, or `~/.silvana-agent` when it is unset
  (`--control-dir` overrides both). It holds one folder per network and party. Back it up with
  the `.env`, and keep it on persistent storage.
- The machine's clock must be within 60 seconds of the rpc's. The agent refuses to sign a control
  entry otherwise.

## Timing rules

- Every controller change (link, add, remove, capabilities) takes effect **24 hours after the agent
  signs it**. `control show` lists pending changes with their effective time.
- Withdrawing a grant that has not taken effect yet applies at once, so a mistaken add can be undone.
- A controller with a pending removal keeps its capabilities until the removal takes effect.
- A pending controller can sign nothing.
- From the first link on, the agent checks every transaction against what it asked for, hashes
  every transaction locally, and refuses `--force`, `--no-reject` and the `sign` command.

## Linking

### At onboarding

An invite code from the Agent page can carry a link part: `AGT-<invite>-<link>`. Onboard as usual:

```bash
./cloud-agent onboard --agent-name my-agent --email me@example.com --invite-code AGT-XXXX-YYYY
```

Only `AGT-<invite>` is sent for registration. Once the agent is onboarded, it shows the owner
account and the controllers of the invite and asks you to type `link`. It then prints the party
fingerprint: check it on the Agent page to confirm the agent.

Without a terminal, nothing is linked. Ask the owner for a link code for the agent's party and run
`control link` (below).

### An existing agent

```bash
./cloud-agent control link --code LNK-XXXX
```

The agent fetches the code's descriptor, checks that it matches the code, that it names this
agent's party and network, and that it has not expired. It shows the owner account and, for each
controller, its type, label, fingerprint, capabilities and, for a passkey, its relying party,
origins and backup policy. Type `link` to sign.

`control link` is refused while any controller is in effect or pending: use `control add` then.

## Adding, removing and changing controllers

```bash
# A controller from an add code made on the Agent page
./cloud-agent control add --code LNK-XXXX

# A local Ed25519 key
./cloud-agent control add --ed25519 <64 hex> --label "Backup key" --capabilities policy,direct,read

# Remove a controller, by label or by at least 8 hex characters of its id
./cloud-agent control remove "Backup key"

# Change its capabilities
./cloud-agent control capabilities "Backup key" read
```

Each command shows the change and its effective time and asks for a typed `add`, `remove` or
`change`. `--yes` skips that question.

**The minimum rule.** A change that would leave a capability without any controller, counting every
pending change, is refused. It names the capability and the controllers that would remain. With
`--force` the change is allowed, after a warning that names the policy left in force. You must then
type the phrase it shows, such as `freeze agent-1 default policy`. `--yes` is never accepted with
`--force`. A forced removal of every controller followed by a new link takes at least 48 hours.

## Inspecting

```bash
./cloud-agent control show            # controllers, pending changes, policy, head, marker
./cloud-agent control show --offline  # the local copy only, no connection
./cloud-agent control show --json
./cloud-agent control policy show [--pending] [--json]
```

`control show` also lists `PARTY_FEE` and `PARTY_PREPAID_TRAFFIC` as set in the agent's own
environment, and the passkey relying-party ids the agent accepts. When either party is unset, no
payment is treated as a fee or a traffic purchase.

## Keeping the copies in step

Before every change the agent compares its log with the rpc's copy:

- if the rpc's copy is behind, the agent uploads the missing entries;
- if the rpc's copy is ahead or differs, the agent refuses, prints both heads and reports the
  problem. Find out which copy is genuine before doing anything else.

```bash
./cloud-agent control sync
```

`control sync` runs the same comparison on its own and settles a pending entry.

### Pending entries

The agent signs an entry, records it as pending, uploads it, and appends it only once the rpc
accepts it. If the reply is lost, `control sync` finds the entry on the rpc and appends it. If the
rpc refuses it, the entry is kept and its sequence number is not signed again automatically.

```bash
./cloud-agent control discard-pending
```

releases such an entry after you type `discard <seq>`. A copy is kept under `evidence/`. If anyone
kept the refused entry, signing a different one at the same sequence number later shows two
signatures at one position, which reads as a fork. Discard only an entry the rpc does not hold.

## Recovery

The agent refuses to start when `AGENT_CONTROL_HEAD` is set and the control directory is missing,
does not verify, or is behind the marker. Without the marker, the agent starts as before when the
rpc cannot be read; it refuses only when the rpc holds a log signed by this agent's key that the
local copy lacks.

```bash
./cloud-agent control restore
```

rebuilds the local copy from the rpc's log after checking every signature and policy document.
It refuses a log that is behind the marker or differs from it, or from the local copy. Type the
phrase it shows, `restore <seq>:<first 8 hex of the head>`, to confirm the head being restored.

## Ed25519 controller keys

`cloud-agent controller` needs no agent configuration and no network.

```bash
# A new key in its own directory (0700, key file 0600); prints the public key and fingerprint
./cloud-agent controller keygen --dir ~/controller-keys/backup

# Sign a policy, command or read-session document for a network
./cloud-agent controller sign --key ~/controller-keys/backup/controller.key \
    --payload <base64url> --network devnet
./cloud-agent controller sign --key ~/controller-keys/backup/controller.key \
    --file policy.txt --network mainnet --current-policy current.txt
```

`sign` decodes the document and refuses anything but a policy, a command or a read session for
the given network. It shows the policy's rules (and, with `--current-policy`, what it widens), the
command's operation and summary, or the read session's agents and expiry. After you type `sign`, it
prints the Ed25519 signature over the full document as 128 lowercase hex characters.
