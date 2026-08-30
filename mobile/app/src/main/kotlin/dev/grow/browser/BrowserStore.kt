package dev.grow.browser

import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/** A bookmarked page. */
data class Bookmark(val url: String, val title: String)

/** A history entry: when `url` was visited under `title`. */
data class HistoryEntry(val url: String, val title: String, val visitedAt: Long)

/**
 * Bookmarks and history, persisted as JSON files in the app's files dir.
 * History is capped at [MAX_HISTORY] entries, newest first.
 */
class BrowserStore(private val dir: File) {

    private val bookmarksFile = File(dir, "bookmarks.json")
    private val historyFile = File(dir, "history.json")

    fun bookmarks(): List<Bookmark> =
        readArray(bookmarksFile).let { array ->
            (0 until array.length()).map { i ->
                val obj = array.getJSONObject(i)
                Bookmark(obj.getString("url"), obj.optString("title"))
            }
        }

    fun isBookmarked(url: String): Boolean = bookmarks().any { it.url == url }

    /** Bookmark `url`; removes the bookmark instead when already present. */
    fun toggleBookmark(url: String, title: String): Boolean {
        val current = bookmarks()
        val exists = current.any { it.url == url }
        val updated = if (exists) {
            current.filterNot { it.url == url }
        } else {
            current + Bookmark(url, title.ifEmpty { url })
        }
        writeArray(
            bookmarksFile,
            JSONArray().apply {
                updated.forEach {
                    put(JSONObject().put("url", it.url).put("title", it.title))
                }
            },
        )
        return !exists
    }

    fun history(): List<HistoryEntry> =
        readArray(historyFile).let { array ->
            (0 until array.length()).map { i ->
                val obj = array.getJSONObject(i)
                HistoryEntry(
                    obj.getString("url"),
                    obj.optString("title"),
                    obj.optLong("visited_at"),
                )
            }
        }

    fun recordVisit(url: String, title: String) {
        val entry = HistoryEntry(url, title.ifEmpty { url }, System.currentTimeMillis())
        val updated = (listOf(entry) + history().filterNot { it.url == url }).take(MAX_HISTORY)
        writeArray(
            historyFile,
            JSONArray().apply {
                updated.forEach {
                    put(
                        JSONObject()
                            .put("url", it.url)
                            .put("title", it.title)
                            .put("visited_at", it.visitedAt),
                    )
                }
            },
        )
    }

    private fun readArray(file: File): JSONArray =
        runCatching { JSONArray(file.readText()) }.getOrDefault(JSONArray())

    private fun writeArray(file: File, array: JSONArray) {
        file.writeText(array.toString())
    }

    companion object {
        private const val MAX_HISTORY = 500
    }
}
