package dev.grow.browser

import org.json.JSONObject

/**
 * JNI bridge to the Rust `grow-mobile` engine (libgrow_mobile.so).
 *
 * The engine owns the registry directory, the tokio runtime, and one
 * localhost server per resolved app. All three native calls are synchronous;
 * [resolve] can take seconds on first use (network clone) and must be called
 * off the main thread.
 */
class Engine private constructor() {

    data class ResolveResult(
        val ok: Boolean,
        val url: String?,
        val app: String?,
        val commit: String?,
        val port: Int?,
        val error: String?,
    ) {
        companion object {
            fun fromJson(raw: String): ResolveResult {
                val json = JSONObject(raw)
                return ResolveResult(
                    ok = json.optBoolean("ok"),
                    url = json.optString("url").ifEmpty { null },
                    app = json.optString("app").ifEmpty { null },
                    commit = json.optString("commit").ifEmpty { null },
                    port = if (json.has("port")) json.optInt("port") else null,
                    error = json.optString("error").ifEmpty { null },
                )
            }
        }
    }

    companion object {
        init {
            System.loadLibrary("grow_mobile")
        }

        @JvmStatic
        private external fun nativeStart(filesDir: String): String

        @JvmStatic
        private external fun nativeResolve(url: String): String

        @JvmStatic
        private external fun nativeApps(): String

        /** Start the engine; idempotent. `filesDir` is the app's files dir. */
        fun start(filesDir: String) {
            nativeStart(filesDir)
        }

        /** Resolve a grow:// URL to a localhost URL (blocking). */
        fun resolve(url: String): ResolveResult = ResolveResult.fromJson(nativeResolve(url))
    }
}
