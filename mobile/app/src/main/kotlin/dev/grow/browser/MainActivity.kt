package dev.grow.browser

import android.annotation.SuppressLint
import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Color
import android.os.Bundle
import android.view.View
import android.view.ViewGroup
import android.view.inputmethod.EditorInfo
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.PopupMenu
import android.widget.Toast
import java.util.concurrent.Executors

/**
 * Grow Browser: `grow://` is a first-class scheme.
 *
 * The address bar accepts grow:// URLs (resolved by the embedded Rust
 * engine — clone, verify, serve on a device-local port) as well as plain
 * https:// URLs. One WebView per tab; bookmarks and history persist in the
 * app's files directory via [BrowserStore].
 */
class MainActivity : Activity() {

    /** One browser tab: its WebView plus the URL shown in the address bar. */
    private class Tab(val id: Long, val webView: WebView) {
        var displayUrl: String = ""
        var title: String = ""
        // Set when the tab is showing a resolved grow app: the address bar
        // keeps showing the grow:// URL while the WebView is on 127.0.0.1.
        var growBase: String? = null
        var growPort: Int? = null
    }

    private lateinit var addressBar: EditText
    private lateinit var progress: ProgressBar
    private lateinit var webContainer: FrameLayout
    private lateinit var store: BrowserStore

    private val tabs = LinkedHashMap<Long, Tab>()
    private var currentTabId: Long = 0
    private var nextTabId = 1L
    private val executor = Executors.newSingleThreadExecutor()
    private var engineReady = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        store = BrowserStore(filesDir)
        engineReady = runCatching { Engine.start(filesDir.absolutePath) }.isSuccess
        buildLayout()
        handleIntent(intent)
        if (tabs.isEmpty()) openTab("")
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleIntent(intent)
    }

    private fun handleIntent(intent: Intent?) {
        val url = intent?.dataString ?: return
        navigate(url)
    }

    // ---- UI -----------------------------------------------------------

    @SuppressLint("SetJavaScriptEnabled")
    private fun buildLayout() {
        addressBar = EditText(this).apply {
            hint = "grow://github.com/owner/repo or https://…"
            setSingleLine()
            imeOptions = EditorInfo.IME_ACTION_GO
            setOnEditorActionListener { _, _, _ ->
                navigate(text.toString())
                true
            }
        }
        progress = ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal).apply {
            isIndeterminate = true
            visibility = View.GONE
        }
        webContainer = FrameLayout(this)

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(
                LinearLayout(context).apply {
                    orientation = LinearLayout.HORIZONTAL
                    addView(addressBar, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                    addView(iconButton("+") { openTab("") })
                    addView(iconButton("★") { toggleBookmark() })
                    addView(iconButton("⋮") { showMenu(it) })
                },
            )
            addView(progress, ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            addView(webContainer, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
        }
        setContentView(root)
    }

    private fun iconButton(label: String, onClick: (View) -> Unit): Button =
        Button(this).apply {
            text = label
            setOnClickListener(onClick)
        }

    private fun showMenu(anchor: View) {
        val popup = PopupMenu(this, anchor)
        popup.menu.add("Tabs (${tabs.size})")
        popup.menu.add("Bookmarks")
        popup.menu.add("History")
        popup.setOnMenuItemClickListener { item ->
            when (item.title.toString()) {
                "Bookmarks" -> showBookmarks()
                "History" -> showHistory()
                else -> showTabs()
            }
            true
        }
        popup.show()
    }

    // ---- Tabs ---------------------------------------------------------

    private fun currentTab(): Tab? = tabs[currentTabId]

    private fun openTab(url: String) {
        val webView = WebView(this).apply {
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            webViewClient = tabClient
        }
        val tab = Tab(nextTabId++, webView)
        tabs[tab.id] = tab
        switchToTab(tab.id)
        if (url.isEmpty()) showHome(webView) else navigate(url)
    }

    private fun switchToTab(id: Long) {
        val tab = tabs[id] ?: return
        currentTabId = id
        webContainer.removeAllViews()
        webContainer.addView(tab.webView, ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
        addressBar.setText(tab.displayUrl)
        addressBar.setSelection(addressBar.text.length)
    }

    private fun closeTab(id: Long) {
        val tab = tabs.remove(id) ?: return
        tab.webView.destroy()
        if (tabs.isEmpty()) {
            openTab("")
        } else if (currentTabId == id) {
            switchToTab(tabs.keys.last())
        }
    }

    private fun showTabs() {
        val list = tabs.values.toList()
        val labels = list.map { tab -> tab.title.ifEmpty { tab.displayUrl.ifEmpty { "New tab" } } }.toTypedArray()
        AlertDialog.Builder(this)
            .setTitle("Tabs")
            .setItems(labels) { _, which -> switchToTab(list[which].id) }
            .setNeutralButton("Close current") { _, _ -> closeTab(currentTabId) }
            .setNegativeButton("Cancel", null)
            .show()
    }

    // ---- Navigation ---------------------------------------------------

    private fun navigate(input: String) {
        val url = input.trim()
        if (url.isEmpty()) return
        when {
            url.startsWith("grow://") -> resolveGrow(url)
            url.startsWith("http://") || url.startsWith("https://") -> loadHttp(url)
            url.contains(' ') -> toast("Enter a grow:// or https:// URL")
            url.contains('.') -> loadHttp("https://$url")
            else -> toast("Enter a grow:// or https:// URL")
        }
    }

    private fun resolveGrow(growUrl: String) {
        if (!engineReady) {
            showError(growUrl, "The grow engine library is not loaded in this build.")
            return
        }
        progress.visibility = View.VISIBLE
        executor.execute {
            val result = Engine.resolve(growUrl)
            runOnUiThread {
                progress.visibility = View.GONE
                val tab = currentTab() ?: return@runOnUiThread
                if (result.ok && result.url != null) {
                    tab.growBase = growUrl.trimEnd('/')
                    tab.growPort = result.port
                    tab.displayUrl = growUrl
                    addressBar.setText(growUrl)
                    tab.webView.loadUrl(result.url)
                } else {
                    showError(growUrl, result.error ?: "resolution failed")
                }
            }
        }
    }

    private fun loadHttp(url: String) {
        val tab = currentTab() ?: return
        tab.growBase = null
        tab.growPort = null
        tab.displayUrl = url
        addressBar.setText(url)
        tab.webView.loadUrl(url)
    }

    private val tabClient = object : WebViewClient() {
        override fun shouldOverrideUrlLoading(view: WebView, url: String): Boolean {
            if (url.startsWith("grow://")) {
                navigate(url)
                return true
            }
            return false
        }

        override fun onPageFinished(view: WebView, url: String) {
            val tab = tabs.values.find { it.webView == view } ?: return
            val port = tab.growPort
            val base = tab.growBase
            if (port != null && base != null && url.startsWith("http://127.0.0.1:$port")) {
                // Navigation within the served grow app: display the grow URL.
                val path = url.removePrefix("http://127.0.0.1:$port").trimEnd('/')
                tab.displayUrl = if (path.isEmpty() || path == "/") base else "$base$path"
            } else {
                tab.growBase = null
                tab.growPort = null
                tab.displayUrl = url
            }
            tab.title = view.title ?: ""
            if (tab.id == currentTabId) {
                addressBar.setText(tab.displayUrl)
                addressBar.setSelection(addressBar.text.length)
            }
            if (tab.displayUrl.isNotEmpty()) {
                store.recordVisit(tab.displayUrl, tab.title)
            }
        }
    }

    private fun showHome(webView: WebView) {
        val bookmarks = store.bookmarks()
        val items = if (bookmarks.isEmpty()) {
            "<li><em>no bookmarks yet</em></li>"
        } else {
            bookmarks.joinToString("") { "<li><a href=\"${it.url}\">${it.title}</a></li>" }
        }
        webView.loadData(
            "<!doctype html><html><body style=\"font-family:sans-serif;margin:2rem\">" +
                "<h2>Grow Browser</h2>" +
                "<p>Enter <code>grow://github.com/&lt;owner&gt;/&lt;repo&gt;</code> above.</p>" +
                "<h3>Bookmarks</h3><ul>$items</ul></body></html>",
            "text/html",
            "utf-8",
        )
    }

    private fun showError(url: String, message: String) {
        val tab = currentTab() ?: return
        tab.webView.loadData(
            "<!doctype html><html><body style=\"font-family:sans-serif;margin:2rem\">" +
                "<h3>Could not resolve ${htmlEscape(url)}</h3><p>${htmlEscape(message)}</p></body></html>",
            "text/html",
            "utf-8",
        )
    }

    // ---- Bookmarks & history ------------------------------------------

    private fun toggleBookmark() {
        val tab = currentTab() ?: return
        if (tab.displayUrl.isEmpty()) return
        val added = store.toggleBookmark(tab.displayUrl, tab.title)
        toast(if (added) "Bookmarked" else "Bookmark removed")
    }

    private fun showBookmarks() {
        val bookmarks = store.bookmarks()
        if (bookmarks.isEmpty()) {
            toast("No bookmarks yet")
            return
        }
        val labels = bookmarks.map { it.title }.toTypedArray()
        AlertDialog.Builder(this)
            .setTitle("Bookmarks")
            .setItems(labels) { _, which -> navigate(bookmarks[which].url) }
            .setNegativeButton("Close", null)
            .show()
    }

    private fun showHistory() {
        val history = store.history()
        if (history.isEmpty()) {
            toast("No history yet")
            return
        }
        val labels = history.take(100).map { "${it.title} — ${it.url}" }.toTypedArray()
        AlertDialog.Builder(this)
            .setTitle("History")
            .setItems(labels) { _, which -> navigate(history[which].url) }
            .setNegativeButton("Close", null)
            .show()
    }

    // ---- Misc ---------------------------------------------------------

    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
        val tab = currentTab()
        when {
            tab == null -> super.onBackPressed()
            tab.webView.canGoBack() -> tab.webView.goBack()
            tabs.size > 1 -> closeTab(tab.id)
            else -> super.onBackPressed()
        }
    }

    override fun onDestroy() {
        tabs.values.forEach { it.webView.destroy() }
        tabs.clear()
        super.onDestroy()
    }

    private fun toast(message: String) = Toast.makeText(this, message, Toast.LENGTH_SHORT).show()

    private fun htmlEscape(text: String): String =
        text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
}
