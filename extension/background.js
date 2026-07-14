// grow extension background service worker.
//
// Chrome cannot navigate to unknown schemes like grow:// directly, so the
// omnibox keyword, the popup, and grow:// link clicks all end up here: the
// local grow daemon resolves the URL (installing and serving the app on
// first use) and the tab is sent to the returned localhost address.

const RESOLVER = "http://127.0.0.1:7890";

function normalizeInput(text) {
  const trimmed = text.trim();
  if (trimmed.startsWith("grow://")) return trimmed;
  return "grow://" + trimmed.replace(/^\/+/, "");
}

async function resolveGrowUrl(growUrl) {
  const resp = await fetch(`${RESOLVER}/resolve?u=${encodeURIComponent(growUrl)}`);
  const data = await resp.json().catch(() => ({}));
  if (!resp.ok) {
    throw new Error(data.error || `resolver returned HTTP ${resp.status}`);
  }
  if (!data.url) throw new Error("resolver returned no URL");
  return data.url;
}

async function navigate(tabId, growUrl) {
  try {
    const url = await resolveGrowUrl(growUrl);
    await chrome.tabs.update(tabId, { url });
  } catch (err) {
    // The daemon's status page renders the #error banner; if the daemon is
    // down entirely, Chrome's own connection error shows instead.
    const message = String((err && err.message) || err);
    await chrome.tabs.update(tabId, {
      url: `${RESOLVER}/#error=${encodeURIComponent(message)}&u=${encodeURIComponent(growUrl)}`,
    });
  }
}

chrome.omnibox.onInputChanged.addListener((text, suggest) => {
  const growUrl = normalizeInput(text);
  suggest([
    {
      content: growUrl,
      description: `Open <match>${growUrl.replace(/</g, "&lt;")}</match> — cloned, verified, and served locally`,
    },
  ]);
});

chrome.omnibox.onInputEntered.addListener((text) => {
  const growUrl = normalizeInput(text);
  chrome.tabs.query({ active: true, currentWindow: true }, (tabs) => {
    if (tabs[0]) navigate(tabs[0].id, growUrl);
  });
});

chrome.runtime.onMessage.addListener((msg, sender) => {
  if (msg && msg.type === "grow:navigate" && sender.tab) {
    navigate(sender.tab.id, msg.url);
  }
});
