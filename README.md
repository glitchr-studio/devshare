# DevShare

Share one or several local development environments with another computer for
a few minutes, under their own hostnames and ports, over an encrypted
peer-to-peer link. Nothing is deployed, no port is opened on the sharing
machine, and nothing else on it becomes reachable.

```text
$ devshare share ~/Sites/shop

  shop
    shop.test:8443
    shop.test:5173

Invitation:
https://join.glitchr.dev/#gVOxtm5P6ALN7P…

Code:
7GX2-KLM9
```

The guest runs `sudo devshare join <invitation>` and opens `https://shop.test:8443`
in a browser, as if the project ran on their own machine. When the session
expires or the host stops it, the names and the routes are gone.

## Status

Early and experimental. It works between macOS and Linux computers and is
covered by automated tests, but:

- the invitation scheme has not had a security review;
- the iPhone app is built but not distributed, and there is no Android or
  Windows client yet: a phone that opens an invitation gets a page saying so;
- by default, sessions go through the public relays of the
  [iroh](https://github.com/n0-computer/iroh) project;
- joining a session needs either the helper below, installed once with
  `sudo`, or `sudo devshare join`.

## Licence

GNU General Public License, version 3 or later: see [LICENSE](LICENSE).

## How it works

- **The host** reads which ports a project publishes from its Docker Compose
  file (`devshare discover`, or automatically on the first `devshare share`)
  and writes them to a `devshare.toml` in the project's folder. Databases,
  caches, mail servers and shells are left out unless asked for. The
  hostname is the one the project gives itself in its environment
  (`SSL_CERT_DOMAINS=localhost,shop.local` in `.env` or `.env.local` makes it
  `shop.local`; also `SERVER_NAME`, `VIRTUAL_HOST`, `APP_DOMAIN`,
  `DEFAULT_URI`, `APP_URL`), else `<folder>.test`. Behind a
  reverse proxy, the names it answers come from its configuration: Traefik
  labels, a Caddyfile, nginx's `server_name`, and their subdomains in
  `/etc/hosts`. Without a compose file, a Vite dev server and the Symfony
  CLI's server are found from their own configuration.
- **The invitation** is one link, also shown as a QR code. It carries the
  host's identity for the session and the relay it is reachable through.
- **The link** between host and guest is made by iroh: both sides connect
  outwards to a relay, then go direct when their networks allow it. Traffic is
  encrypted end to end.
- **The guest** gets a temporary network interface and name resolution for the
  shared hostnames only. Each connection it opens is checked by the host
  against the list of shared services; everything else is refused.

The page at `join.glitchr.dev` is the single static file in [docs/](docs/). The
invitation is in the part of the address after the `#`, which a browser never
sends: the page reads it on the device, and no server is involved.

## Try it

Docker and a Rust toolchain are needed (`brew install rustup` on macOS).

```sh
make install          # builds the commands into bin/
source env.sh         # puts bin/ on the PATH of this shell
make demo             # starts the example project and shares it
```

`make demo` runs the small project in [tests/showcase](tests/showcase) and
prints its invitation: a link, and the same link as a QR code. Keep that
terminal open; Ctrl-C stops the session. Then join it from another device,
as below, and open `http://showcase.test:8710`.

## Testing on each kind of device

The link works from any network: the devices connect directly when their
networks allow it, through a relay otherwise. Nothing needs to be opened on
either side's router. In the terminal of `make demo`, `guests` lists who is
connected and `revoke <number>` disconnects one.

### A computer: macOS or Linux

1. Install DevShare on it. On a Mac, as above: `make install`, then
   `source env.sh`. For Linux, `make deb` (on any machine with Docker)
   builds packages into `dist/`; on the Linux computer:
   `sudo apt install ./devshare_<version>_amd64.deb` (or `_arm64`).
2. Once, `sudo devshare-helper install`: from then on, joining needs no
   sudo. Without it, join with `sudo devshare join …` each time.
3. `devshare join '<invitation>'`. It says `Connected.`, lists what is
   shared, and whether the link is direct or relayed.
4. Open `http://showcase.test:8710` in any browser, or
   `curl http://showcase.test:8710`.
5. Ctrl-C leaves. `showcase.test` stops resolving at once.

To check HTTPS without a warning, run `devshare ca install` once (on macOS
it asks for your password), then join a session that shares an HTTPS
service: `devshare join` marks it `certified by this device`, and the
browser opens it without a warning.

The desktop app does the same on a Mac: `make dmg`, copy
`dist/DevShare.app` to Applications, then paste the invitation under "Join
a session", or open the invitation link in the browser and click "Open in
the DevShare app".

Windows is not supported yet.

### An iPhone or an iPad

The app is not in the App Store yet: it is installed from a Mac with Xcode,
on a device with iOS 17 or later and Developer Mode on (Settings, Privacy &
Security, Developer Mode).

1. On the Mac: `make ios-app`, then open `ios/DevShare.xcodeproj`.
2. Connect the device, choose it at the top of Xcode's window, and press
   Run (⌘R). The app opens on the device.
3. In the app, tap **Scan a QR code** and scan the code `make demo`
   printed, or paste the link. Use the app's own scanner: the Camera app
   opens the invitation page in Safari instead, which can only show the
   code.
4. Tap **Join**, then `showcase.test:8710`: it opens in the app's browser.

Only the app's own browser reaches the shared names; Safari and the other
apps of the device do not. HTTPS services are accepted when their
certificate is the one the host saw, with nothing to install.

### An Android phone or tablet

There is no Android app yet. Scanning the QR code with the camera opens the
invitation page in the browser, which shows the code and says so: that page
is all an Android device can check today.

### A tablet

An iPad uses the iPhone app, as above. An Android tablet is in the same
place as an Android phone.

## Joining without sudo

A guest's computer needs administrator rights for one thing: creating the
session's network interface and pointing the system's resolver at it. The
helper does that and nothing else, hands the interface to your own process,
and removes the names when your process ends, however it ends:

```sh
sudo devshare-helper install     # once; serves the user who runs it
devshare join <invitation>       # from then on, no sudo
```

It runs at boot, listens on a Unix socket, and serves only the users recorded
at installation (`sudo devshare-helper install --user <name>` adds one). It
refuses names outside the test domains unless root lists a domain in
`/etc/devshare/trusted-domains`. `sudo devshare-helper uninstall` removes it.

## HTTPS without a warning

A project's `https://shop.test` has a certificate no browser of the guest
trusts. A guest can make a certificate authority of their own device, once:

```sh
devshare ca install
```

From then on, in the sessions they join, the guest's device answers
`https://shop.test` with a certificate it makes for that name and that
session, and reaches the host's service with a client that accepts only the
certificate the host saw when it shared it. Browsers and every other program
open the page without a warning, and nothing of the host's is trusted. A
service whose certificate changed during the session is refused.

The authority is trusted on this device only, and can only vouch for
development names: its certificate is constrained to `.test`, `.localhost`,
`.example`, `.invalid`, `.internal`, `home.arpa`, `.local` and the guest's own `domain`
setting, and excludes every IP address. On Linux, the helper installs it in
the system's store and refuses one that is not constrained so. On macOS,
the system lets no background service change trust settings: `devshare ca
install` trusts it in your login keychain, for SSL only, and macOS asks for
your password once. Its key is a file readable by the user alone, in DevShare's data
folder. `devshare ca` shows it, `devshare ca renew` replaces it, `devshare ca
remove` removes it; uninstalling the helper removes every one it installed.
Firefox on Linux keeps its own list of authorities and needs it imported by
hand.

## The desktop app

`app/` is the same thing with a window (Tauri). It lists the projects
found in the folders it looks in (shown at the top, `~/Sites` and the usual
ones to start with: add or remove any) and the ones added by hand (a folder,
or a `devshare.toml` of any name), one line each, with a switch: what is
switched on is shared when you press Share, for the time you choose or with
no time limit. Any project can be taken off the list and put back; the ones
with nothing to share are left off unless put back. A
project switched on but not running gets a Start button, which runs its own
`make up` when its Makefile has one (else `docker compose up -d`, or the
`up = "…"` of its `devshare.toml`); Stop runs `make down` the same way.
While sharing: the invitation, who is connected by their computer's login,
disconnect someone, stop. In a sidebar (Settings): the helper (installed
with the system's administrator dialog), the device's certificate authority
(install, renew, remove) and the general settings, edited in place: no
terminal needed. It also joins: paste an invitation, or click "Open in the
DevShare app" on an invitation page, which hands it over through a
`devshare://` link; the app shows it and waits for a click, a page can never
make it join on its own. `make app` runs it, `make dmg` builds an unsigned
`dist/DevShare.app` and disk image; the `devshare://` link only works once
the app is in Applications.

## The iPhone app

`ios/` holds the iPhone and iPad app (SwiftUI, iOS 17 or later) on the same
core, through `mobile/` (UniFFI). It joins with the invitation (pasted,
scanned, or handed over by a `devshare://` link) and opens the services in
its own browser, under their real names. That browser goes through a proxy
on the phone's loopback, guarded by a password: the session's names go
through the session, with HTTPS checked against the certificate the host
saw, and any other site is reached directly. Other apps of the phone are not
affected; a system-wide tunnel needs Apple's Network Extension and is not
written yet.

```sh
make ios-app      # the core for iOS, the bindings, ios/DevShare.xcodeproj
```

Then open `ios/DevShare.xcodeproj` in Xcode, pick an iPhone and run.

## Commands

| Command | What it does |
|---|---|
| `devshare share [folder…]` | Shares the projects in these folders, the current one by default. `--only <name>` keeps some environments, `--duration 15m` (or `none`: until you stop it) and `--guests 3` set the limits. While sharing: `guests`, `revoke <n>`, `invite`. |
| `devshare join <invitation>` | Joins a session with its link, its QR code's text or its short code. |
| `devshare discover [folder]` | Writes or refreshes the folder's `devshare.toml` from its compose file. |
| `devshare settings` | Shows the general settings, common to all projects (`~/.config/devshare/devshare.toml`). |
| `devshare ca` | This device's own certificate authority: `install`, `renew`, `remove`, see above. |
| `devshare-helper` | `install`, `uninstall`, `run`: the part that needs root on a guest's computer, see above. |
| `devshare-server` | A control plane to run on its own. Not needed for local use: `share` starts one when none is running. |

## The public server

Sessions work without one: the invitation link carries everything a guest
needs, and both sides reach each other through a relay. A public server adds
a relay of one's own, instead of the public relays of the iroh project, and a
control plane, so that the short code typed by hand works from any network.
Both run at one name, behind Caddy, from [deploy/](deploy/):

```sh
# on the server, which the name points at, with TCP 80 and 443 and UDP 7842 open
make deploy DEVSHARE_HOST=relay.join.glitchr.dev
```

Clients then use it with two lines in `~/.config/devshare/devshare.toml`:

```toml
server = "https://relay.join.glitchr.dev"
relay = "https://relay.join.glitchr.dev"
```

The control plane never sees an invitation code: it stores a slow key derived
from it, which lets a guest find a host but not join it. `deploy/test.sh`
checks the same routing locally, over plain HTTP.

## Layout

```text
protocol/   manifest, invitation code, messages between guest and host
core/       the link, the host agent, the guest tunnel, compose discovery
cli/        devshare
server/     the control plane: devshare-server
app/        the desktop app (Tauri)
mobile/     the core for the phone apps, through UniFFI
ios/        the iPhone and iPad app (SwiftUI)
docs/       the public invitation page, served at join.glitchr.dev
deploy/     the public server: relay and control plane behind Caddy
docker/     end-to-end harness: host, guest, control plane, relay
tests/      showcase/: an example compose project
```

## Tests

```sh
make tests    # unit and session tests, in Docker
make check    # lint
make e2e      # whole sessions between containers: direct, relayed, and with a
              # guest that can reach nothing but a relay
```
