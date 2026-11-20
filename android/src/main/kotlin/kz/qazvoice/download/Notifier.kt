package kz.qazvoice.download

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.SystemClock
import java.util.Locale

/** Builds the download notifications (texts come from `QazNotificationConfig`). */
class Notifier(private val ctx: Context) {
    companion object {
        const val CHANNEL = "qazvoice_downloads"
        const val ONGOING_ID = 0x51a2
        private const val MIN_INTERVAL_MS = 1000L
    }

    private val textsFile = java.io.File(ctx.filesDir, "qazvoice/notification.json")
    private val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

    @Volatile var texts: Map<String, String> = mapOf(
        "channelName" to "Model downloads",
        "downloading" to "Downloading {name}",
        "progress" to "{done} of {total} · {speed}",
        "waitingNetwork" to "Waiting for network",
        "waitingWifi" to "Waiting for Wi-Fi",
        "paused" to "{name}: paused",
        "done" to "{name} is ready",
        "failed" to "{name}: download failed",
        "pause" to "Pause",
        "resume" to "Resume",
        "cancel" to "Cancel",
        "retry" to "Retry",
        "units" to "B,KB,MB,GB",
        "perSecond" to "/s",
    )
        set(value) {
            field = field + value
            // Kept on disk: a service restarted by the system has no Dart side to ask.
            runCatching { textsFile.writeText(org.json.JSONObject(field).toString()) }
            ensureChannel(force = true)
        }



    @Volatile private var lastUpdate = 0L
    @Volatile var current: Notification? = null
        private set

    init {
        runCatching {
            if (textsFile.exists()) {
                val o = org.json.JSONObject(textsFile.readText())
                texts = texts + o.keys().asSequence().associateWith { o.getString(it) }
            }
        }
    }

    private fun t(key: String, vararg kv: Pair<String, String>): String {
        var s = texts[key] ?: key
        for ((k, v) in kv) s = s.replace("{$k}", v)
        return s
    }

    private fun ensureChannel(force: Boolean = false) {
        if (Build.VERSION.SDK_INT < 26) return
        if (!force && nm.getNotificationChannel(CHANNEL) != null) return
        val ch = NotificationChannel(CHANNEL, t("channelName"), NotificationManager.IMPORTANCE_LOW)
        ch.setShowBadge(false)
        nm.createNotificationChannel(ch)
    }

    @Suppress("DEPRECATION")
    private fun builder(): Notification.Builder {
        ensureChannel()
        val b = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(ctx, CHANNEL) else Notification.Builder(ctx)
        ctx.packageManager.getLaunchIntentForPackage(ctx.packageName)?.let { launch ->
            b.setContentIntent(PendingIntent.getActivity(ctx, 0, launch, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT))
        }
        return b.setOnlyAlertOnce(true).setShowWhen(false)
    }

    private fun action(action: String, id: String, foreground: Boolean): PendingIntent {
        val i = Intent(ctx, DownloadService::class.java).setAction(action).putExtra(DownloadService.EXTRA_ID, id)
        val flags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        val code = (action + id).hashCode()
        return if (foreground && Build.VERSION.SDK_INT >= 26) PendingIntent.getForegroundService(ctx, code, i, flags)
        else PendingIntent.getService(ctx, code, i, flags)
    }

    @Suppress("DEPRECATION")
    private fun Notification.Builder.act(icon: Int, title: String, pi: PendingIntent) = addAction(Notification.Action.Builder(icon, title, pi).build())

    private fun idFor(model: String) = 0x51a3 + (model.hashCode() and 0xffff)

    /** The ongoing notification the service runs under. */
    fun ongoing(): Notification = current ?: builder()
        .setSmallIcon(android.R.drawable.stat_sys_download)
        .setContentTitle(t("downloading", "name" to ""))
        .setOngoing(true)
        .setProgress(0, 0, true)
        .build().also { current = it }

    fun progress(spec: JobSpec, received: Long, total: Long, speed: Double) {
        val now = SystemClock.elapsedRealtime()
        if (now - lastUpdate < MIN_INTERVAL_MS && received < total) return
        lastUpdate = now
        val pct = if (total > 0) (received * 1000 / total).toInt() else 0
        val n = builder()
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle(t("downloading", "name" to spec.name))
            .setContentText(t("progress", "done" to size(received), "total" to size(total), "speed" to "${size(speed.toLong())}${t("perSecond")}",
                "eta" to eta(total - received, speed)))
            .setProgress(1000, pct, false)
            .setOngoing(true)
            .act(android.R.drawable.ic_media_pause, t("pause"), action(DownloadService.ACTION_PAUSE, spec.id, false))
            .act(android.R.drawable.ic_menu_close_clear_cancel, t("cancel"), action(DownloadService.ACTION_CANCEL, spec.id, false))
            .build()
        post(ONGOING_ID, n, ongoing = true)
        nm.cancel(idFor(spec.id))
    }

    fun waiting(spec: JobSpec, reason: String, received: Long, total: Long) {
        val n = builder()
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle(t("downloading", "name" to spec.name))
            .setContentText(t(if (reason == "wifi") "waitingWifi" else "waitingNetwork"))
            .setProgress(1000, if (total > 0) (received * 1000 / total).toInt() else 0, false)
            .setOngoing(true)
            .act(android.R.drawable.ic_media_pause, t("pause"), action(DownloadService.ACTION_PAUSE, spec.id, false))
            .act(android.R.drawable.ic_menu_close_clear_cancel, t("cancel"), action(DownloadService.ACTION_CANCEL, spec.id, false))
            .build()
        lastUpdate = 0
        post(ONGOING_ID, n, ongoing = true)
    }

    fun paused(spec: JobSpec, received: Long) {
        val n = builder()
            .setSmallIcon(android.R.drawable.stat_sys_download_done)
            .setContentTitle(t("paused", "name" to spec.name))
            .setContentText("${size(received)} / ${size(spec.downloadBytes)}")
            .setProgress(1000, if (spec.downloadBytes > 0) (received * 1000 / spec.downloadBytes).toInt() else 0, false)
            .act(android.R.drawable.ic_media_play, t("resume"), action(DownloadService.ACTION_RESUME, spec.id, true))
            .act(android.R.drawable.ic_menu_close_clear_cancel, t("cancel"), action(DownloadService.ACTION_CANCEL, spec.id, true))
            .build()
        nm.cancel(ONGOING_ID)
        post(idFor(spec.id), n, ongoing = false)
    }

    fun done(spec: JobSpec) {
        val n = builder()
            .setSmallIcon(android.R.drawable.stat_sys_download_done)
            .setContentTitle(t("done", "name" to spec.name))
            .setAutoCancel(true)
            .build()
        nm.cancel(ONGOING_ID)
        post(idFor(spec.id), n, ongoing = false)
    }

    fun failed(spec: JobSpec, why: String) {
        val n = builder()
            .setSmallIcon(android.R.drawable.stat_notify_error)
            .setContentTitle(t("failed", "name" to spec.name))
            .setContentText(why.take(120))
            .setAutoCancel(true)
            .act(android.R.drawable.ic_popup_sync, t("retry"), action(DownloadService.ACTION_RESUME, spec.id, true))
            .build()
        nm.cancel(ONGOING_ID)
        post(idFor(spec.id), n, ongoing = false)
    }

    fun clear(model: String) {
        nm.cancel(idFor(model))
    }

    fun clearOngoing() {
        nm.cancel(ONGOING_ID)
        current = null
    }

    private fun post(id: Int, n: Notification, ongoing: Boolean) {
        if (ongoing) current = n
        runCatching { nm.notify(id, n) } // no POST_NOTIFICATIONS permission: silently skipped
    }

    private fun size(b: Long): String {
        val u = t("units").split(',').map { it.trim() }.let { if (it.size == 4) it else listOf("B", "KB", "MB", "GB") }
        return when {
            b >= 1L shl 30 -> String.format(Locale.ROOT, "%.1f %s", b / 1073741824.0, u[3])
            b >= 1L shl 20 -> String.format(Locale.ROOT, "%.1f %s", b / 1048576.0, u[2])
            b >= 1L shl 10 -> String.format(Locale.ROOT, "%.0f %s", b / 1024.0, u[1])
            else -> "$b ${u[0]}"
        }
    }

    private fun eta(left: Long, speed: Double): String {
        if (speed <= 0) return ""
        val s = (left / speed).toLong()
        return if (s >= 3600) "${s / 3600}:%02d:%02d".format(Locale.ROOT, s % 3600 / 60, s % 60) else "%d:%02d".format(Locale.ROOT, s / 60, s % 60)
    }
}
