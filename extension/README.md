# grow Chrome Extension

Navigate `grow://` URLs — sites hosted on git repositories (GitHub and
GitLab) and served locally by the grow runtime.

```
grow://github.com/<owner>/<repo>[/path/inside/the/site]
grow://gitlab.com/<owner>/<repo>[/path/inside/the/site]
```

## How it works

Chrome cannot navigate to unknown schemes, so the extension asks the local
grow daemon to resolve them:

1. `grow daemon` runs a control plane on `127.0.0.1:7890`.
2. The extension (omnibox keyword, popup, or a `grow://` link click) sends
   the URL to `GET /resolve?u=...`.
3. The daemon clones the repo over https (first use only), verifies HEAD,
   installs it into the local registry, serves it on a per-app localhost
   port with the usual approval and storage policy, and answers with the
   localhost URL.
4. The tab navigates to the served site. Once resolved, ordinary relative
   links work as usual; `grow://` links are intercepted and resolved the
   same way.

Repositories are never auto-updated: if the remote moves on, the installed
commit keeps serving until you run `grow install <url>` again — the same
trust model as the CLI.

## Install

1. Build and install grow, then start the daemon:

   ```sh
   cargo build --release
   ./target/release/grow daemon   # listens on 127.0.0.1:7890
   ```

2. Open `chrome://extensions`, enable **Developer mode**, choose
   **Load unpacked**, and select this `extension/` directory.

## Usage

- **Address bar**: type `grow`, press Tab, then
  `github.com/elci-group/octo-site` and Enter.
- **Links**: any `<a href="grow://...">` on any page is intercepted and
  resolved (left- and middle-click).
- **Popup**: click the grow toolbar icon — it shows daemon health, the apps
  currently being served, and a direct URL input.

The daemon's status page is at http://127.0.0.1:7890/ (served apps, plus an
error banner when the extension reports a failed resolve).

## Requirements

- The `grow` binary (the daemon spawns per-app servers in-process) and a
  `git` binary on the host (https clones are made through it).
- Only public repositories on `github.com` and `gitlab.com`; credentials in
  URLs are rejected by design. GitLab subgroups are not supported yet.
