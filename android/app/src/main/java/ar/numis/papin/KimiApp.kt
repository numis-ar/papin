package ar.numis.papin

import android.app.Application
import ar.numis.papin.di.AppContainer
import ar.numis.papin.service.NotificationHelper

class PapinApp : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)
        NotificationHelper.ensureChannels(this)
    }
}
