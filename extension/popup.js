const RESOLVER = "http://127.0.0.1:7890";

const statusEl = document.getElementById("status");
const errorEl = document.getElementById("error");
const appsEl = document.getElementById("apps");
const form = document.getElementById("open-form");
const input = document.getElementById("url");

function normalize(text) {
  const trimmed = text.trim();
  return trimmed.startsWith("grow://") ? trimmed : "grow://" + trimmed.replace(/^\/+/, "");
}

function showError(message) {
  errorEl.textContent = message;
  errorEl.style.display = "block";
}

async function refresh() {
  try {
    const health = await fetch(`${RESOLVER}/healthz`);
    if (!health.ok) throw new Error(`HTTP ${health.status}`);
    statusEl.textContent = "daemon running — 127.0.0.1:7890";
    statusEl.className = "ok";
  } catch {
    statusEl.textContent = "daemon not running — start it with: grow daemon";
    statusEl.className = "down";
  }
  try {
    const resp = await fetch(`${RESOLVER}/apps`);
    const data = await resp.json();
    const apps = data.apps || [];
    appsEl.innerHTML = apps.length
      ? apps.map((a) => `<li><a href="${a.url}">${a.app}</a> — :${a.port}</li>`).join("")
      : "<li><em>none</em></li>";
  } catch {
    appsEl.innerHTML = "<li><em>unavailable</em></li>";
  }
}

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  errorEl.style.display = "none";
  const growUrl = normalize(input.value);
  try {
    const resp = await fetch(`${RESOLVER}/resolve?u=${encodeURIComponent(growUrl)}`);
    const data = await resp.json().catch(() => ({}));
    if (!resp.ok) throw new Error(data.error || `resolver returned HTTP ${resp.status}`);
    await chrome.tabs.create({ url: data.url });
    window.close();
  } catch (err) {
    showError(String((err && err.message) || err));
  }
});

refresh();
