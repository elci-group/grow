// Intercepts clicks on grow:// links (including middle-click aux events) and
// routes them through the background worker, which asks the local grow
// daemon to resolve and serve the target.
function handle(event) {
  const link = event.target && event.target.closest
    ? event.target.closest('a[href^="grow://"]')
    : null;
  if (!link) return;
  event.preventDefault();
  event.stopPropagation();
  chrome.runtime.sendMessage({ type: "grow:navigate", url: link.getAttribute("href") });
}

document.addEventListener("click", handle, true);
document.addEventListener("auxclick", handle, true);
