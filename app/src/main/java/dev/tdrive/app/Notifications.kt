package dev.tdrive.app

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat

/// Best-effort transfer notifications. A notification channel is required on
/// Oreo+ (minSdk 26). Mirrors the desktop client's notify() helper, which is
/// silent when notifications aren't available.
object Notifications {
    private const val CHANNEL_ID = "tdrive_transfers"
    private const val NOTIFICATION_ID = 1001

    fun ensureChannel(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        val channel = NotificationChannel(
            CHANNEL_ID,
            "Transfers",
            NotificationManager.IMPORTANCE_DEFAULT,
        ).apply { description = "Transfer completion notifications" }
        manager.createNotificationChannel(channel)
    }

    fun post(context: Context, title: String, text: String) {
        val builder = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_sys_download_done)
            .setContentTitle(title)
            .setContentText(text)
            .setPriority(NotificationCompat.PRIORITY_DEFAULT)
            .setAutoCancel(true)
        try {
            NotificationManagerCompat.from(context).notify(NOTIFICATION_ID, builder.build())
        } catch (_: SecurityException) {
            // POST_NOTIFICATIONS not granted — best effort, like the desktop's
            // silent notify-send fallback.
        }
    }
}
