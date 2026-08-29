# sdr-ais

Receives [AIS](https://en.wikipedia.org/wiki/Automatic_identification_system) vessel position broadcasts on 161.975 / 162.025 MHz with an RTL-SDR and serves them as a live JSON feed.

There are two ways to run it.

**Local** — `sdr-ais` binds the dongle directly and serves a Leaflet map on `localhost:3000`.
This is the development workflow; no sandbox, no vsock.

**Public** — the decoder and the web server run inside a Firecracker microVM with no network device, fed raw radio samples from the host over vsock, and published to the internet through Tailscale Funnel.
That is what the rest of this document is about.

## Why a microVM

The API is exposed to the open internet, so two pieces of code have to handle input from people we do not trust:

- the **web server**, which parses HTTP from anyone who finds the URL, and
- the **AIS decoder**, which parses radio frames from anyone with a VHF transmitter in range.

Both run inside the microVM. The host keeps only the parts that cannot be sandboxed — the USB dongle — and does no parsing at all.

The natural split would have been to decode on the host and send tidy JSON into the VM.
We deliberately did not do that.
Decoding on the host means a long-lived privileged process running a DSP chain and a protocol parser over attacker-supplied RF, which is precisely the code you would most like to have contained.
Shipping raw I/Q instead reduces the host to a byte pump.

## How it fits together

```
                                     ┌─────────── Firecracker microVM ───────────┐
                                     │  no network device · read-only rootfs     │
   RTL-SDR                           │  no shell · no libc · one static binary   │
      │ USB                          │                                           │
      ▼                              │            ┌──────────────┐               │
 ┌──────────┐   raw CU8 I/Q          │   pipe     │ rs162 demod  │               │
 │   tap    │───────────────────────▶│──▶ fd 0 ──▶│ AIS decode   │               │
 └──────────┘   vsock host:5000      │            │ vessel table │               │
                                     │            └──────┬───────┘               │
 ┌──────────┐                        │                   ▼                       │
 │  proxy   │◀───────────────────────│───────────  axum HTTP/WS                  │
 └────┬─────┘   vsock guest:8000     │             (AF_VSOCK :8000)              │
      │                              └───────────────────────────────────────────┘
      │ 127.0.0.1:8080
      ▼
 ┌──────────────────┐
 │ tailscale funnel │  terminates TLS, forwards plaintext to loopback
 └────────┬─────────┘
          │ :443
          ▼
     the internet
```

Three host processes, all unprivileged:

| Process | Job | Reads from the guest? |
|---|---|---|
| `sdr-ais-host tap` | Configures the dongle, copies raw samples to the guest | **No** — write-only |
| `firecracker` | Runs the microVM | n/a |
| `sdr-ais-host proxy` | Bridges loopback TCP to the guest's vsock listener | Yes, as opaque bytes |

Neither host binary contains a parser.

## How the vsock plumbing actually works

A Firecracker guest has no USB and no NIC. Its only channel to the host is a virtio-vsock device, which Firecracker exposes on the host side as a **Unix domain socket** — not as a host `AF_VSOCK` socket, which is the part that usually trips people up. `socat` cannot talk to it directly, because each direction has its own small convention.

**Host → guest** (used by the HTTP proxy). Connect to the base socket, then announce which guest port you want:

```
connect  /run/user/1000/sdr-ais/vsock.sock
send     "CONNECT 8000\n"
recv     "OK 1234\n"          ← guest had a listener; raw bytes follow
                              ← immediate EOF means it did not
```

**Guest → host** (used by the I/Q feed). The guest connects to CID 2 (`VMADDR_CID_HOST`) on port 5000. Firecracker responds by connecting to a Unix socket whose path is the base path with the port appended:

```
/run/user/1000/sdr-ais/vsock.sock_5000
```

so the tap simply listens there. There is no handshake in this direction — that is why the I/Q feed uses it, and why the tap can be write-only.

Inside the guest, `AIS_SAMPLE_RATE` is 288 kHz complex-u8, so the feed runs at 576 KB/s. Firecracker's vsock handles far more than that.

### Two details worth knowing

**Why the guest rewires its own stdin.** `rs162` can only take I/Q from a file, a TCP socket, or stdin — there is no constructor that accepts an arbitrary reader, and the guest has no network stack to offer it a TCP socket. Since the guest binary is PID 1, it creates a pipe and `dup2`s the read end onto its own fd 0, then feeds that pipe from vsock. `AisAsyncIqSource::from_stdin` is then satisfied, and the VM still contains no network stack at all.

**Why reconnects pad a byte.** The I/Q stream is unframed interleaved CU8: two bytes per sample, I then Q. If the tap restarts mid-sample, the guest has received an orphan I byte, and every subsequent sample would be read with I and Q transposed. The demodulator does not report this as an error — it just silently stops decoding, forever. So the guest counts bytes per connection and writes one padding byte when a connection ends on an odd count.

## The guest filesystem

```
/init    the static binary (1.4 MB), PID 1
/proc    mounted at boot, to read the kernel command line
/dev     empty
```

That is the entire userland. There is no shell, no `busybox`, no dynamic loader, and the root filesystem is mounted read-only. Firecracker is started with `--no-api`, so there is no control-plane socket to reconfigure the VM once it is running.

Runtime configuration arrives on the kernel command line (`sdr.origin`, `sdr.iq_port`, `sdr.http_port`), so changing the allowed origin needs a VM restart rather than a rebuild.

## Setup

### 1. Host preparation (once, needs root)

```bash
sudo ./scripts/setup-host.sh
```

The RTL-SDR Blog V3 is electrically a DVB-T television receiver and reports USB ID `0bda:2838`, so Linux binds `dvb_usb_rtl28xxu` to it on plug-in. A USB interface can only have one driver, so userspace cannot claim the device until that one lets go. The script blacklists the DVB modules, installs a udev rule giving the `plugdev` group access to the raw USB node, and enables systemd lingering so the VM survives you logging out.

If the dongle was already plugged in, replug it afterwards.

### 2. Build

```bash
./scripts/build.sh
```

Builds the host binaries, builds the guest statically against musl, and assembles the root filesystem with `mke2fs -d` — which populates an image from a directory without mounting it, so no root is needed.

The guest kernel is any uncompressed Firecracker `vmlinux` with `CONFIG_VIRTIO_VSOCKETS` and `CONFIG_EXT4_FS`. If `vm/vmlinux` is missing the script will reuse one from `~/src/firecracker-getting-started/`, or tell you where to fetch one.

### 3. Run

```bash
./scripts/vm-up.sh      # start tap, microVM and proxy
./scripts/vm-down.sh    # stop all three, clean up sockets
```

`vm-up.sh` waits for the guest to answer `/healthz` and prints the logs if it does not. Logs live in `/run/user/$UID/sdr-ais/`.

### 4. Publish

```bash
./scripts/funnel-up.sh    # expose to the internet
./scripts/funnel-down.sh  # withdraw
```

Kept separate from `vm-up.sh` because going public should be a deliberate act.

## The public URL

The frontend talks to the node's own MagicDNS name:

```
https://alnitak.tail36e7a.ts.net
```

**`sdr-ais.alnitak.cxcc.nl` is deliberately not used**, because Tailscale Funnel cannot serve it.

Funnel only serves names inside your tailnet's domain, and only on ports 443, 8443 and 10000. The certificate Tailscale provisions covers `alnitak.tail36e7a.ts.net` and nothing else. A `CNAME` from `sdr-ais.alnitak.cxcc.nl` would resolve, but the certificate presented would not match the name the browser asked for, so the connection would be refused before any request was sent. There is no Funnel setting that changes this.

The accepted trade-off is that the tailnet name becomes public and there is no rate limiting in front of the service. If either becomes a problem, the fix is to put Cloudflare in front: move `cxcc.nl` to Cloudflare and proxy `sdr-ais.alnitak.cxcc.nl` to origin `alnitak.tail36e7a.ts.net` with SSL mode *Full* — not *Full (Strict)*, since the origin certificate deliberately will not match. That restores the custom domain and adds a WAF and rate limiting, at the cost of a DNS migration off Openprovider.

Running Caddy on the host with a DNS-01 certificate and a forwarded port 443 would also work, but it publishes your home IP address and opens an inbound port, which is a materially worse blast radius than Funnel's outbound-only tunnel.

### Funnel prerequisites

Funnel needs two things enabled in the Tailscale admin console before `funnel-up.sh` will succeed:

- **DNS** → MagicDNS and HTTPS Certificates both on.
- **Access controls** → the `funnel` node attribute granted to this node:

  ```json
  "nodeAttrs": [
    { "target": ["100.92.0.43"], "attr": ["funnel"] }
  ]
  ```

  The `target` field accepts `*`, a user, a group, a tag, or a tailnet IP — **not** a MagicDNS name. Using `alnitak.tail36e7a.ts.net` there fails with `invalid address`. `100.92.0.43` scopes Funnel to this node alone; `bcleenders@github` would grant it to every device you own.

Note that enabling HTTPS certificates publishes the hostname to public Certificate Transparency logs. That is inherent to Let's Encrypt, not specific to Tailscale.

## API

Read-only. No writes, no path parameters, no query parameters, no request bodies.

| Endpoint | Returns |
|---|---|
| `GET /healthz` | `ok` |
| `GET /v1/vessels` | JSON array — most recent position per vessel |
| `GET /ws` | WebSocket; the current table, then a live message per decoded position |

`/ws` sends the same JSON objects your existing `index.html` already consumes, so the frontend needs no changes beyond pointing at the new host. It also replays the current vessel table on connect, so a fresh client renders immediately instead of waiting for each ship's next transmission.

## Security notes

What the sandbox does and does not buy you, stated plainly.

**Contained.** A bug in the HTTP layer or the AIS decoder gets an attacker a process inside a VM with no network device, a read-only disk, no shell to exec, and one vsock socket that leads to a write-only byte pump. Escaping requires a Firecracker or KVM vulnerability.

**Bounded.** The vessel table evicts least-recently-heard entries past 4096, so a malfunctioning transmitter cannot grow guest memory without limit. Streaming clients are capped at 64, proxy connections at 128, request bodies at 0 bytes, and responses at a 30-second timeout.

**Origin checking is not authentication.** `/v1/vessels` sets CORS for the frontend origin, and `/ws` checks `Origin` by hand because browsers do not preflight WebSocket upgrades. Both stop *other websites* from reading the feed in a user's browser. Neither stops `curl`, which can send any `Origin` it likes — requests with no `Origin` header are allowed through precisely so command-line debugging works. This is public vessel data, so that trade is deliberate; if the feed ever carries something private it needs real authentication instead.

**Not covered.** Funnel publishes your node's MagicDNS name, and there is no rate limiting in front of the proxy — an attacker who finds the URL can open connections up to the caps above. Cloudflare (option 2 above) would add both rate limiting and origin concealment.
