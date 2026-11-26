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
- The agent's `.env` must be known (the one in the working directory, `--config-dir <DIR>`, or
  `--env-file`). The agent records the head of its control log there as
  `AGENT_CONTROL_HEAD=<network>:<seq>:<hash>`.
- The agent keeps its control store in `control/` in its agent folder, the directory of its `.env`
  (see [Where the state lives](#where-the-state-lives)). Keep that folder on persistent storage.
- The machine's clock must be within 60 seconds of the rpc's. The agent refuses to sign a control
  entry otherwise.

## Where the state lives

The **agent folder** is the directory of the env file the agent loads: `--env-file`, else
`DIR/.env` under `--config-dir DIR`, else the `.env` found in the working directory or its nearest
parent; with no env file it is the working directory. Onboarding writes `./.env`, or the `--env-file` it is given, so the folder is set from
the start. An env file that is a symlink belongs to the folder of its target, where the marker is
written.

| Path | What it holds |
| --- | --- |
| `.env` | the key, the settings and the marker `AGENT_CONTROL_HEAD` |
| `control/` | `log.json`, `head.json`, `pending.json`, `policies/`, `evidence/`, `commands.json`, `counters.json`, their locks, and `moved_from.json` after a move |
| `usdcx/<network>/<party>/` | the `usdcx` burns and deposits |

- One folder holds one agent. A control store whose files name another party or network than the
  agent's is refused, and so is registration evidence of another key.
- `control show` prints the store's path (`storeDir` with `--json`).
- A backup, a copy or a move to another machine must take the **whole agent folder**: the marker
  in `.env` and the store in `control/` belong together. An agent whose marker is ahead of its
  store refuses to start; `control restore` rebuilds the store from the rpc.
- `SILVANA_CONTROL_DIR`, or `control --control-dir`, names another folder to hold `control/`.
  It no longer holds one folder per network and party: if you set it before, move
  `<dir>/<network>/<party>/control` to `<dir>/control` yourself.

### Moving from `~/.silvana-agent`

Earlier versions kept every store in `~/.silvana-agent/<network>/<party>/control/`. When the agent
or a `control` command opens the store, `control/` does not exist yet and the old store does, it
moves the old store once, holding a lock on the agent folder and the old store's lock files:

1. It copies the old store to `control.migrating/` in the agent folder, replacing an unfinished
   copy of an earlier attempt.
2. It checks the copy as at startup: the chain, the head, the pending entry, the policies and,
   when `AGENT_CONTROL_HEAD` is set, the marker. The copy's head must be the marker's, neither
   behind nor ahead of it.
3. It records the old path in `moved_from.json`, renames the copy to `control/` and logs one line
   naming both paths.

A copy that fails a check is removed and nothing starts; the message gives the reason. The old
store is never changed, renamed or deleted. It stays behind as a stale copy, which an older build
refuses once the marker moves past it. Delete `~/.silvana-agent` yourself once every agent has
started from its own folder.

If `control/` exists but was not moved from the old store, and the old store exists too, the agent
refuses and names both paths; it never picks one. Keep the right one and move the other out of both
places. `control restore` moves an old store without the marker check, then rebuilds it from the
rpc.

Agents that shared one folder with one env file each (`--env-file agent1.env`, `--env-file
agent2.env`) need a folder each: move each env file into its own folder before the first start. In
a shared folder the first agent to start moves its store in, and the next one is refused because
`control/` holds another agent's files.

`usdcx` commands move this party's state from `~/.silvana-agent/usdcx/<network>/<party>/` to
`usdcx/<network>/<party>/` in the agent folder in the same way, unless `--state-dir` is given. The
state of other parties there is left alone.

## Timing rules

- Every controller change (link, add, remove, capabilities) takes effect **exactly 1 hour (devnet)
  or 24 hours (mainnet) after the agent signs it**. The network the entry is signed for sets the
  delay; no setting changes it. `control show` lists pending changes with their effective time.
- Before you confirm, a command shows about when the change takes effect. Once the rpc accepts the
  signed entry, it prints the signature time and the exact effective time.
- Withdrawing a grant that has not taken effect yet applies at once, so a mistaken add can be undone.
- A controller with a pending removal keeps its capabilities until the removal takes effect.
- A pending controller can sign nothing.
- A policy's command scopes may name only controllers the log has recorded, which is checked when
  the policy is submitted and accepted rather than on replay, because a scope only restricts.
- From the first link on, the agent checks every transaction against what it asked for, hashes
  every transaction locally, and refuses `--force`, `--no-reject` and the `sign` command.
- These checks read contracts by template name, so they assume genuine packages. Only a policy
  whose packages rule lists the vetted packages, instead of allowing any, or a `[vetted_packages]`
  selection in agent.toml, enforces that.

## Linking

### At onboarding

An invite code from the Agent page can carry a link part: `AGT-<invite>-<link>`. Onboard as usual,
in a terminal:

```bash
./cloud-agent onboard --network devnet --agent-name my-agent --invite-code AGT-XXXX-YYYY
```

`--network` is `mainnet` (the default) or `devnet`, and picks that network's rpc; `testnet` is
refused, as it has no rpc yet. No email is sent: the account that owns the invite code has it.

Only `AGT-<invite>` is sent for registration. Once the agent is onboarded, it shows the owner
account and the controllers of the invite, then what to check on the Agent page: under Create
agent, the invite's row shows the same party fingerprint, owner account and passkey fingerprint.
It then asks you to type `link` and waits 15 min: this is the only chance to link with the invite.
After the link it prints the party fingerprint again.

The link part is used only when `onboard` registers the agent (not with `--party`, nor when the env
file already records `PARTY_AGENT`), and only once you type `link`. Without a terminal nothing is
linked, and a later `onboard` does not link the agent either: it then cannot be linked yet (below).

### An existing agent

An agent that is already onboarded cannot be linked yet. `control link` needs a link code (`LNK-…`)
that names the agent's party, and the Agent page does not make one: it shows only agents that are
already linked, and makes invite codes for new agents and add codes for linked ones.

```bash
./cloud-agent control link --code LNK-XXXX
```

Given such a code, the agent fetches the code's descriptor, checks that it matches the code, that it
names this agent's party and network, and that it has not expired. It shows the owner account and,
for each controller, its type, label, fingerprint, capabilities and, for a passkey, its relying
party, origins and backup policy. Type `link` to sign.

`control link` is refused while any controller is in effect or pending: use `control add` then.

### Platform parties

A linked agent signs a fee payment only to `PARTY_FEE` and a prepaid traffic top-up only to
`PARTY_PREPAID_TRAFFIC`, as its own env names them. While one is unset it refuses those payments
(and, without `PARTY_FEE`, any swap quote with a fee), and the agent logs a warning that says what
it refuses: at startup, or when it becomes linked while running. A running linked agent takes a pin
that is unset from its own env file on its next poller round, and never replaces one that is set.

The ledger service names both parties, when it has them, in the agent config it serves. Onboarding
refuses a config that names one that is not a party id, writes each one only when neither the
process environment nor the env file sets it, and prints how both stand. `control link` and the link
step of onboarding read the agent config for
a party that is still unset, show it before the typed confirmation and write it once the link is
signed and uploaded. A value that is set is never replaced or removed, so a different value served
later is not used: change one only by hand, from a trusted source.

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
`--force`. A forced removal of every controller followed by a new link takes at least two delays:
2 hours on devnet, 48 hours on mainnet.

## Inspecting

```bash
./cloud-agent control show            # controllers, pending changes, policy, head, marker
./cloud-agent control show --offline  # the local copy only, no connection
./cloud-agent control show --json
./cloud-agent control policy show [--pending] [--json]
```

`control show` also lists `PARTY_FEE` and `PARTY_PREPAID_TRAFFIC` as set in the agent's own
environment, the `[control]`, `[vetted_packages]` and `[verify]` settings, and the passkey relying-party ids
the agent accepts. When either party is unset, no payment is treated as a fee or a traffic purchase,
so a linked agent refuses them (see [Platform parties](#platform-parties)).

## Policies

A `policy` controller signs a policy on the Agent page or with `controller sign`; the rpc holds it
as pending until the agent accepts it into its control log.

```bash
./cloud-agent control policy show --pending       # each pending policy, as the agent judges it
./cloud-agent control policy accept               # the only pending policy
./cloud-agent control policy accept --hash <64 hex>
```

`policy accept` checks the policy against the agent's own verified log: network, party, a signer
holding `policy` now, the next version on the latest policy, its validity, the proof, and that every
command scope names a recorded controller. It shows the rules, what the policy widens against the
rules in force (transfers, trading, other, packages, scopes), the package changes and when it takes
effect, then asks for a typed `accept policy v<N>`. `--yes` skips that question only when nothing
widens. A policy that fails a check is answered with a refusal the agent signs, which the rpc and the
Agent page show.

- Narrowing applies at once and cancels a widening that is still scheduled.
- A widening waits: transfers, trading and other wait the delays of the rules in force; packages and
  command scopes wait the controller delay, 1 hour on devnet and 24 hours on mainnet.
- A packages rule that selects packages must cover every package a transaction's nodes use: each
  template's package and the interface package of each exercise or fetch made through an interface
  (input contracts are not judged). A CC transfer, for one, also uses
  `splice-api-token-transfer-instruction-v1` and `splice-api-token-holding-v1` besides
  `splice-amulet`. A linked agent checks the rule in force before every signature, a direct
  command's or its own, and refuses a transaction outside it with `package_not_allowed` (reason 19
  in a direct command's receipt).

### The agent's own polling

While the agent runs (`cloud-agent agent`) and is linked, it lists its pending policies and commands
every 60 seconds. It accepts each valid pending policy, or refuses an invalid one, without a prompt.
Pending direct commands are taken one at a time, oldest first, and only in a round that settled
every listed policy: while one waits for `control policy accept` (see below), is issued later than
this clock allows, cannot be read or fails to settle, or more than 8 are listed, the agent runs no
command in that round. `control execute` is not held this way. With `auto_execute_commands = true`
the agent runs each valid one. Otherwise each waits for `cloud-agent control execute`, and only a
command that can never run (malformed, badly signed, expired, for another agent, network or log head,
or from an unknown controller) is answered with a refusal. A `usdcx_burn` command is refused at once: the
agent does not run burns yet. The agent re-reads its control log before each step, never polls under
`--dry-run`, and an agent that is not linked does nothing here.

### agent.toml

```toml
[control]
auto_execute_commands = false   # run pending direct commands without a prompt (default false)
auto_accept_policies = true     # accept valid pending policies while running (default true)
swap_max_deviation_bps = 100    # largest gap between a swap's price and the reference mid; 0 = off; at most 10000
swap_reference_max_age_secs = 300 # oldest reference mid the guard takes, in seconds; 30 to 3600

[vetted_packages]
mode = "selected"               # or "all", with no packages key (the default)
packages = [
  "<64 lowercase hex package id>", # name version
]

[verify]
mode = "default"                # or "observe" or "enforce"
```

The sections are optional and read strictly: an unknown key, a value out of range, or a key of
`[control]`, `[vetted_packages]`, `[controller]` or `[verify]` placed at the top level of agent.toml,
or below the header of another table such as `[liquidity_provider]` or `[[markets]]` (TOML adds a
key to the table above it), stops the agent with a message naming the section, the key and where it
is. The running agent reads them only when it starts, so restart it after a change; each `control`
command reads them when it runs. With `auto_accept_policies = false`, pending policies wait for
`control policy accept`, as they do when the agent knows no env file to record the marker in.

`[vetted_packages]` takes `all`, or `selected` with 1 to 256 different package ids; the Agent page's
Packages tab copies the section. With `selected`, every agent, linked or not, checks every
transaction before it signs it: each template's package and the interface package of each exercise
or fetch must be listed, or the transaction is refused unsigned with `package_not_allowed`, logged
with the package's name and version when the agent knows them or the rpc lists them (it asks at
most once a minute). A linked
agent also checks the packages rule of the policy in force, so a transaction passes only when both
allow it. With a selection, local or the policy's, the agent signs only when its own hash of the
transaction matches the server's, so the bytes it checked are the bytes it signs; a hash that does
not match or cannot be computed is refused unsigned with `package_not_allowed`. `all` checks
nothing.

`[verify]` sets how the verifier's newer checks act while the agent is not linked: `default` takes
each operation's own setting (the newer checks of most operations observe for now), `observe` logs
and counts what enforcing would refuse and signs as before, and `enforce` refuses. For an operation
whose fields are checked, enforcing also refuses a transaction whose hash does not match or cannot
be computed, where observing signs the server's hash and counts it, unless `[vetted_packages]`
selects packages. A check that refused before refuses in every mode, the USDCx operations and
accepting a DvP always need a matching hash, and a linked agent enforces everything whatever the
setting.

## Direct commands

A `direct` controller signs a command on the Agent page or with `controller command`; the rpc holds
it as pending until the agent answers it with a receipt it signs.

```bash
./cloud-agent control commands                  # the pending commands, oldest first
./cloud-agent control execute --code CMD-<id>   # run one, after a typed confirmation
```

`control commands` lists each pending command with its operation, expiry, signer fingerprint and
local state (`new`, `received`, `executing`, `executed`, `refused` or `failed`), then the commands
whose outcome is not settled yet or whose receipt the rpc has not taken yet.

`control execute` takes the code exactly as the Agent page shows it. It re-reads the control log and
verifies the command against it: network, party, a signer holding `direct` now, the log head the
command names, its validity and proof, and the signer's command scope. It then checks the rules in
force, shows the command (asset, amount and recipient; for a swap, what it sells and buys, the least
it must receive and the fee cap; then the signer, expiry and summary) and asks for a typed phrase:

- a transfer: `transfer <amount> <asset>`, such as `transfer 5 CC` or `transfer 10 USDCx`;
- a swap: `swap <amount> <asset>`, the exact amount and asset it sells, such as `swap 100 CC`.

There is no `--yes`, and `--dry-run` is refused. Each command gets at most one signed attempt:

- A transfer is prepared with the command attached. Right before the signature the agent checks the
  transaction against the command, the rules in force and the packages rule.
- A swap trades on the active market whose two instruments are exactly the ones it names. The agent
  first makes sure it can receive the bought instrument. It then requests RFQ V2 quotes at most
  every 10 seconds, from the LPs the signer's scope lists (any LP when it lists none), until one
  settles or the command is about to expire: at most 120 requests and 5 accepted quotes. A quote is
  taken only when it sells exactly the signed amount, charges fees only in CC to `PARTY_FEE` and at
  most `max_fee_cc`, pays at least the minimum (net of the CC fee when buying CC), and is within
  `swap_max_deviation_bps` of the agent's reference mid. The best net amount wins, then the lower
  fee. The settle is one atomic transaction, bound to the command before its signature. Under a
  policy whose trading rule lists markets instead of allowing any, every swap command is refused
  with reason 9 (`market_not_allowed`).
- When the outcome is not known after the signature, the command is never signed again. Run the
  same `control execute` later, or let the running agent settle it.

`control execute` exits 0 only when the command executed, now or earlier; any other outcome is an
error.

### Receipts

The agent signs one receipt for each command and posts it to the rpc. The Agent page and
`controller command show` treat as verified only a receipt the agent key signed. A receipt the rpc
did not take is kept and posted again by the running agent, or by `control execute` with the same
code.

| Outcome | Meaning |
| --- | --- |
| `executed` | The transaction committed. The receipt (version 1) names its update id. An executed swap's receipt is version 2: it adds the accepted quote, the LP, the amounts sold and received, and the CC fee. |
| `refused` | A check refused the command and it did not run. The code is the agent's (source `agent`): a command, proof or policy reason code, such as 1106 for an expired command or 1108 for a kind outside the signer's scope. Or it is the ledger service's (source `ledger`), relayed as given. |
| `failed` | The command was valid but could not be carried out, and nothing was transferred. The code is one of the table below. |

| Code | Name | Meaning |
| --- | --- | --- |
| 1400 | `no_acceptable_quote` | No active market trades the pair, or no allowed LP quoted before the time ran out |
| 1401 | `below_minimum` | The quotes paid less than the minimum |
| 1402 | `fee_above_cap` | The quotes charged more than `max_fee_cc`, or a fee that is not CC to `PARTY_FEE` |
| 1403 | `inexact_size` | The quotes did not sell exactly the signed amount |
| 1404 | `lp_rejected` | The LP refused or did not complete the accepted quote |
| 1405 | `insufficient_holdings` | The agent's holdings could not cover the sold amount |
| 1406 | `preapproval_unavailable` | The agent could not set up receiving the bought instrument |
| 1407 | `content_refused` | The prepared transaction is not the one the command directs, so it was not signed |
| 1408 | `window_closed` | A quote's signed window, or the command's validity, ended before the settle |
| 1409 | `submit_failed` | The attempt stopped before its signature, or its transaction was not on the ledger 10 minutes after the command expired |
| 1410 | `no_reference_price` | The price guard is on and no reference mid at most `swap_reference_max_age_secs` old (300 seconds by default) was available |
| 1411 | `price_deviation` | The price was worse than the reference mid by more than `swap_max_deviation_bps` |

A `usdcx_burn` command is refused with 1407. When the quotes of a swap failed for different reasons,
the receipt names the one that came closest to settling.

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

The agent refuses to start when `AGENT_CONTROL_HEAD` is set and the control store is missing,
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

`cloud-agent controller keygen` and `controller sign` need no agent configuration and no network.
The online commands below sign in to the rpc with the same key.

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
the given network. It shows the policy's rules (and, with `--current-policy`, the categories it
widens, packages and command scopes included, and its package changes), the command's operation
and summary, or the read session's agents and expiry. After you type `sign`, it
prints the Ed25519 signature over the full document as 128 lowercase hex characters.

### Controlling an agent over the rpc

A machine that holds an Ed25519 controller key, and no agent key, can read and control the agent
the key was added to. It signs in by signing a challenge the rpc issues for this key, this agent,
the network and the rpc's host; the session lasts at most 10 minutes and lives only in memory.
Every policy and command still carries the key's own signature, which the agent checks again.

```toml
# agent.toml on the controller's machine
[controller]
key_file = "controller/controller.key"   # relative to agent.toml
agent_party = "<agent party id>"
# network = "devnet"                     # else CANTON_CHAIN
```

```bash
./cloud-agent controller status            # the log verified here, this key's standing, the policy
./cloud-agent controller log --json
./cloud-agent controller holdings          # also traffic, activity, ledger, packages, commands
./cloud-agent controller policy show [--rules]
./cloud-agent controller policy set --pause
./cloud-agent controller policy set --scope <fingerprint> --scope-commands swap \
    --scope-pair cc,USDCx@<admin>,0.2,1000 --scope-lp <lp party>
./cloud-agent controller policy set --rules rules.txt --base scheduled
./cloud-agent controller command transfer-cc --to <party> --amount 5 --wait
./cloud-agent controller command transfer-token --to <party> --instrument-admin <party> \
    --instrument-id USDCx --amount 10
./cloud-agent controller command swap --sell-admin <party> --sell-id Amulet --sell-amount 100 \
    --buy-admin <party> --buy-id USDCx --min-receive 15 --max-fee-cc 0.5
./cloud-agent controller command show --code CMD-<id> --wait
```

- `--agent`, `--key` and `--network` override the section; the network falls back to
  `CANTON_CHAIN`. The rpc is the network's own; another one must be https (plain http only on
  loopback).
- Controller mode reads only `[controller]`. It never loads an agent key, refuses `--party`,
  `--private-key`, `--quote-private-key`, `--force` and `--confirm`, and refuses the agent named by
  this machine's `PARTY_AGENT`.
- `status`, `log`, `policy` and `command` page the whole log and verify it here: the agent's
  signatures, the chain and every policy document. `holdings`, `traffic`, `activity`, `ledger`,
  `packages` and `commands` show what the rpc reports, without that check. `status` lists any point
  where the rpc's account differs from the log.
- `seen-heads.json`, beside the key file, keeps the longest head this key has verified per agent.
  A shorter log, or another entry at that seq, refuses every signature until the rpc serves the
  log seen before.
- Drafts come from the verified log: a policy is the latest version plus one, chained to its hash,
  valid 1 h by default (`--validity`, at most 24 h); a command names the verified head and is
  valid 10 min (at most 15). Amounts are plain decimals. Each draft is checked as the agent will
  check it before it is signed, and nothing is signed when this clock and the rpc's differ by more
  than 60 s.
- Type `sign policy v<N>` to sign a policy; `--yes` skips it only when the policy widens nothing.
  A command needs `transfer <amount> <asset>` or `swap <amount> <sold> for <minimum> <bought>`
  typed, and never takes `--yes`.
- `--dry-run` shows the draft and its document and signs and sends nothing (the sign-in still
  signs its challenge). `--wait` follows the policy or command until the agent answers; it exits 3
  while it is still pending. A receipt counts only when the agent key signed it for that command:
  a command's wait exits 0 only on such a receipt saying it executed, and 1 on one with any other
  outcome. Without one it exits 3, even when the rpc reports the command expired or settled.
- A scope instrument is `cc` or `ID@ADMIN`. A swap names its instruments by admin and id; the
  agent judges Canton Coin by its own DSO.

## Capturing transactions for tests

Set `SILVANA_CAPTURE_DIR` to a directory to keep a copy of each transaction the agent signs. Right
before each signature the agent appends one JSON line to `<dir>/<YYYYMMDD>.jsonl` (the UTC day):

- `kind`, `path` (`ledger` or `atomic`), `party`, `command_id` and `at` (the time);
- `expectation`, what the agent expects the transaction to do, and `request`, the request it sent
  (protobuf, base64, named by `request_type`), without its signature, a controller's proof or a
  swap quote's signature;
- `prepared_transaction`, the bytes exactly as the ledger service sent them (base64);
- `server_hash_base64` and `server_hash_hex`, `hashing_scheme_version`, `computed_hash_hex` (null
  when none was computed), `hash_check`, `coverage` and `fees_json`.

The agent reads the variable once, at its first signature; unset or blank, nothing is written.
Capturing never delays or refuses a signature: its own thread writes the lines, and a record that
cannot be queued or written is dropped with one warning in the log. A day's file stops growing at
1 GiB. The directory is created with mode 0700 and the files with 0600. The records hold none of
the agent's keys, tokens or signatures, but they do hold party ids, amounts and contract ids, so
keep them private.

Every 10 minutes the agent also logs one `control survey since start:` line with, per operation,
its counters (hash checks, coverage, stub hits, inspector warnings, `inspector_would_refuse`,
`hash_would_refuse`) and the packages its transactions used, by name and
version when the rpc lists them, else by id.
