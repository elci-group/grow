# GROW MVP Demo Roadmap

> **Status: executed 2026-07-14.** All phases D0–D4 complete. Sites live on
> ports 7870/7871/7872; four kaptaind daemons healthy (grow on 3056, demo
> repos on 3057–3059). Findings: kaptaind's version writeback patches only
> `[package].version`, so virtual-workspace repos (grow) drift on every bump
> — grow's thresholds are pinned at 99.0 for the demo; demo repos needed a
> `VERSION` file before the daemon would commit, and their bump threshold
> was lowered to 0.02 for content-scale edits.

This roadmap defines the path to a presentable MVP demo: a working set of
high-quality sites hosted entirely on git repositories, installed and served
by `grow`, with `kaptaind` monitoring both GROW itself and every demo site.

## Demo Goal

A reviewer can watch the full loop without touching project internals:

```sh
grow inspect demos/portal
grow install demos/portal
grow list
grow serve grow-portal --port 7870   # plus field-notes on 7871, pulse on 7872
curl http://127.0.0.1:7870/          # static landing, served from verified HEAD
```

Every served byte comes out of a git commit tree; editing a demo site and
letting kaptaind commit it surfaces as HEAD drift that `grow` refuses to run
until the app is re-installed and re-verified.

## Demo Sites

Each site is its own git repository under `demos/` (ignored by the parent
GROW repo and its kaptaind watch; each has its own kaptaind daemon).

| Site | Repo | Kind | Shows |
|---|---|---|---|
| GROW Portal | `demos/portal` | static | landing page linking all demos, modern responsive CSS, dark mode |
| Field Notes | `demos/field-notes` | static, multi-page | multi-page publication with shared stylesheet, served from HEAD tree |
| Pulse | `demos/pulse` | WASM (WASI p1) | dynamic HTML from a WAT module, request inspection, persistent hit counter on the per-app storage grant |

Pulse deliberately ships no `index.html`: every request reaches the WASM
module. Served from a plain path (dev mode) it reports "storage: not
granted"; installed, it counts hits in its private storage directory.

## Phases

| Phase | Goal | Exit gate |
|---|---|---|
| D0: Hygiene | Land the pending changeset (version alignment), refresh stale README test table and man-page version, ignore `demos/` in parent repo | `scripts/check-version.sh` passes; kaptaind auto-commits the working tree clean |
| D1: Sites | Create the three demo repos with polished content and committed `.grow/manifest.toml` | `grow inspect` is clean for each repo |
| D2: Install & serve | Install all three, serve on ports 7870/7871/7872 | `grow list` shows all three; curl returns 200 with expected content; pulse counter persists across requests |
| D3: Monitoring | kaptaind init + daemon per demo repo; confirm the GROW daemon | `kaptaind-cli status` healthy for all four repos; an edit to a demo site produces a kaptaind auto-commit |
| D4: Verify & present | Full loop: edit → kaptaind commit → HEAD drift refusal → reinstall → updated site | Updated content served; drift error message observed; URLs and evidence collected |

## Execution Rules

- All commits in the GROW repo are made by kaptaind; no manual git mutations.
- Demo repos get one manual bootstrap commit (repo creation); every
  subsequent commit is kaptaind's.
- Demo sites must be dependency-free: inline CSS, no JavaScript required,
  readable without a build step.
- Ports: portal 7870, field-notes 7871, pulse 7872; kaptaind health ports
  3057–3059 (GROW already uses 3056).
