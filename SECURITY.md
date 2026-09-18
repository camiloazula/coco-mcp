# Security

## Reporting a vulnerability

Please do not open a public issue for a security problem. Report it
privately through GitHub's vulnerability reporting for this repository:

https://github.com/camiloazula/coco-mcp/security/advisories/new

You will get an acknowledgement within a few days. Once a fix is ready it
is released with notes that credit the reporter, unless they prefer
otherwise.

## What counts

Coco MCP runs the commands you give it, connects to the servers you point
it at, stores what they return and sends the credentials you configure. A
report is in scope when a server, a file it imports, or a network peer can
make the program do something the user did not ask for: run a program,
open a URL of another scheme, read or write a file outside what the user
chose, leak a token, or hang or exhaust the machine on crafted input. The
README section "What to know before pointing it at a server" lists what is
by design.

## Supported versions

Fixes go to the latest release only. Install with `cargo install --locked`
and update by running the same command again.

## Dependencies

Every dependency is audited against the RustSec database with `cargo deny`
in CI, and Dependabot opens a pull request when an advisory lands on a
crate in `Cargo.lock`.
