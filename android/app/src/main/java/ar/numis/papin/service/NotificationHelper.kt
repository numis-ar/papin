package ar.numis.papin.service

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationCompat
import ar.numis.papin.R
import ar.numis.papin.ui.MainActivity

/**
 * Notifications (PLAN §7): a persistent foreground notification while the ACP
 * WS is held, question notifications for reverse-RPC, and turn-completion
 * notifications. Tapping any of them returns to the affected chat.
 */
object NotificationHelper {
    const val CHANNEL_QUESTIONS = "questions"
    const val CHANNEL_TURNS = "turns"
    const val FG_NOTIFICATION_ID = 1
    const val QUESTION_NOTIFICATION_ID = 2
    const val TURN_NOTIFICATION_ID = 3

    fun ensureChannels(context: Context) {
        val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_QUESTIONS,
                context.getString(R.string.channel_questions),
                NotificationManager.IMPORTANCE_HIGH,
            ),
        )
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_TURNS,
                context.getString(R.string.channel_turns),
                NotificationManager.IMPORTANCE_DEFAULT,
            ),
        )
    }

    fun foregroundNotification(context: Context, agentId: String): Notification {
        return NotificationCompat.Builder(context, CHANNEL_QUESTIONS)
            .setSmallIcon(android.R.drawable.stat_sys_download_done)
            .setContentTitle(context.getString(R.string.fg_service_title))
            .setContentText(context.getString(R.string.fg_service_text))
            .setOngoing(true)
            .setContentIntent(chatPendingIntent(context, agentId, null, FG_NOTIFICATION_ID))
            .build()
    }

    fun questionNotification(context: Context, agentId: String, sessionId: String?, title: String) {
        val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val notification = NotificationCompat.Builder(context, CHANNEL_QUESTIONS)
            .setSmallIcon(android.R.drawable.stat_notify_chat)
            .setContentTitle("Agent question")
            .setContentText(title)
            .setAutoCancel(true)
            .setContentIntent(chatPendingIntent(context, agentId, sessionId, QUESTION_NOTIFICATION_ID))
            .build()
        manager.notify(QUESTION_NOTIFICATION_ID, notification)
    }

    fun turnDoneNotification(context: Context, agentId: String, sessionId: String?) {
        val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val notification = NotificationCompat.Builder(context, CHANNEL_TURNS)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle("Turn finished")
            .setContentText("The agent finished its turn.")
            .setAutoCancel(true)
            .setContentIntent(chatPendingIntent(context, agentId, sessionId, TURN_NOTIFICATION_ID))
            .build()
        manager.notify(TURN_NOTIFICATION_ID, notification)
    }

    private fun chatPendingIntent(
        context: Context,
        agentId: String,
        sessionId: String?,
        requestCode: Int,
    ): PendingIntent {
        val intent = Intent(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            .putExtra(MainActivity.EXTRA_AGENT_ID, agentId)
        sessionId?.let { intent.putExtra(MainActivity.EXTRA_SESSION_ID, it) }
        return PendingIntent.getActivity(
            context,
            requestCode,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }
}
