package ar.numis.papin.ui

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import ar.numis.papin.PapinApp
import ar.numis.papin.ui.nav.AppRoot

class MainActivity : ComponentActivity() {
    companion object {
        const val EXTRA_AGENT_ID = "agent_id"
        const val EXTRA_SESSION_ID = "session_id"
    }

    private val notificationPermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
        val container = (application as PapinApp).container
        setContent {
            AppRoot(
                container = container,
                startAgentId = intent.getStringExtra(EXTRA_AGENT_ID),
                startSessionId = intent.getStringExtra(EXTRA_SESSION_ID),
            )
        }
    }
}
